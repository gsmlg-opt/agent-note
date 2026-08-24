# Building an Emacs Client for Agent Note Org

This guide builds a small Emacs package that edits Agent Note's canonical Org
documents through the REST API. Agent Note remains the source of truth. Emacs
does not synchronize a second Org directory or write directly to the Agent Note
database.

The completed package lets a user:

- select an Agent Note workspace and document with Emacs completion;
- open canonical Org source in a normal `org-mode` buffer;
- create a new Org document in an existing workspace;
- save through the revision-safe REST API with `C-x C-s`;
- reload a document explicitly; and
- inspect the current server version when a concurrent edit causes a conflict.

The reference implementation is intentionally small. It manages Org documents,
not Agent Note's agent-execution operations such as claims, heartbeats, results,
or reviews. Those operations can be added later using the same transport layer.

## 1. Understand the data flow

An Agent Note Org document is database-backed canonical Org text. Opening a
document retrieves its source and revision. Saving sends the complete edited
source together with the revision that Emacs originally read.

```text
Agent Note database
        |
        | GET source + revision
        v
Emacs org-mode buffer
        |
        | PUT source + expected_revision + operation_id
        v
Agent Note validates Org syntax, policy, leases, and revision
```

If another client changes the document first, Agent Note returns HTTP `409` with
the error code `stale_revision`. The package must keep the user's buffer intact
and must not silently overwrite the newer server version.

This is different from the offline export/import workflow. The package below
connects to the running application and uses the same `/api/org` operations as
other REST clients.

## 2. Prerequisites and security boundary

You need:

- GNU Emacs 29.1 or newer;
- the built-in `org`, `org-id`, `url`, and `json` libraries;
- a running Agent Note server; and
- at least one existing, non-archived Org workspace.

For local development, start Agent Note from the repository root:

```sh
cargo run
```

The backend listens on `http://127.0.0.1:6222` by default. Confirm the Org API is
reachable:

```sh
curl --fail --show-error \
  'http://127.0.0.1:6222/api/org/workspaces?limit=1&include_archived=false'
```

Agent Note does not implement inbound authentication or authorization.
`actor_id` is client-asserted audit attribution, not a verified identity. Use a
direct connection only on a trusted network. A remotely reachable deployment
must be protected by an authenticating reverse proxy with TLS and access
control. Choose a nonblank, trimmed actor ID; `system` is reserved by Agent Note.

## 3. REST operations used by the package

| Purpose | Method and path | Important response fields |
| --- | --- | --- |
| List workspaces | `GET /api/org/workspaces` | `items[].workspace_id`, `display_name`, `slug`, `next_cursor` |
| List documents | `GET /api/org/workspaces/{workspace_id}/documents` | `items[].id`, `path`, `revision`, `next_cursor` |
| Read canonical source | `GET /api/org/documents/{document_id}?workspace_id={workspace_id}` | `id`, `workspace_id`, `source`, `path`, `revision`, `content_hash` |
| Create or update a document | `PUT /api/org/documents/{document_id}` | `document_revisions[document_id]` |

List endpoints are cursor-paginated and accept a maximum page size of 200. A
client must treat `next_cursor` as opaque. The package below follows all pages
instead of silently hiding workspaces or documents after the first page.

Every document `PUT` sends this JSON shape:

```json
{
  "schema_version": 1,
  "actor_id": "emacs:gao",
  "operation_id": "6b138434-8f3e-4d0d-a069-7f90bbd52c57",
  "workspace_id": "10000000-0000-4000-8000-000000000001",
  "path": "tasks/inbox.org",
  "source": "#+TITLE: Inbox\n",
  "expected_revision": 7,
  "lease_proofs": {}
}
```

For a new document, `expected_revision` is JSON `null`. For an existing
document, it is the revision returned by the last successful read or write.
`lease_proofs` is an empty JSON object because this human-editing package does
not possess agent lease fencing tokens.

Each logical save needs a unique `operation_id`. If a request times out and the
client cannot tell whether the server committed it, retry the exact same body
with the same operation ID. Do not generate a new operation ID for an ambiguous
retry.

## 4. Create the package

Create the package directory if it does not exist, then create
`~/.emacs.d/lisp/agent-note-org.el`:

```sh
mkdir -p "$HOME/.emacs.d/lisp"
```

Use the following package content:

```emacs-lisp
;;; agent-note-org.el --- Edit Agent Note Org documents via REST -*- lexical-binding: t; coding: utf-8; -*-

;; Version: 0.1.0
;; Package-Requires: ((emacs "29.1"))
;; Keywords: outlines, org, tools

;;; Commentary:

;; Open and edit Agent Note's canonical Org documents through its REST API.
;; This is a minimal reference client, not a filesystem synchronization layer.

;;; Code:

(require 'json)
(require 'org)
(require 'org-id)
(require 'subr-x)
(require 'url)
(require 'url-http)
(require 'url-util)

(defgroup agent-note-org nil
  "Edit Agent Note Org documents through REST."
  :group 'org)

(defcustom agent-note-org-base-url "http://127.0.0.1:6222"
  "Base URL of the Agent Note HTTP server, without an API path."
  :type 'string
  :group 'agent-note-org)

(defcustom agent-note-org-actor-id
  (format "emacs:%s" (user-login-name))
  "Audit actor ID sent with Agent Note mutations.
This value is asserted by the client; it is not authentication."
  :type 'string
  :group 'agent-note-org)

(defcustom agent-note-org-request-timeout 20
  "Maximum seconds to wait for one synchronous HTTP request."
  :type 'number
  :group 'agent-note-org)

(defcustom agent-note-org-extra-headers nil
  "Extra HTTP headers sent to an authenticating reverse proxy.
Each element is a cons cell of the form (HEADER . VALUE).  Agent Note itself
does not define an authentication-header contract.  Do not commit secrets in
this variable to a shared Emacs configuration."
  :type '(alist :key-type string :value-type string)
  :group 'agent-note-org)

(define-error 'agent-note-org-error "Agent Note Org error")
(define-error 'agent-note-org-http-error
  "Agent Note Org HTTP error"
  'agent-note-org-error)

(defvar-local agent-note-org--base-url nil)
(defvar-local agent-note-org--workspace-id nil)
(defvar-local agent-note-org--document-id nil)
(defvar-local agent-note-org--path nil)
(defvar-local agent-note-org--revision nil)
(defvar-local agent-note-org--pending-save nil)

(defvar agent-note-org-buffer-mode-map
  (let ((map (make-sparse-keymap)))
    (define-key map (kbd "C-c C-r") #'agent-note-org-reload)
    (define-key map (kbd "C-c C-v") #'agent-note-org-show-server-version)
    map)
  "Keymap active in REST-backed Agent Note Org buffers.")

(define-minor-mode agent-note-org-buffer-mode
  "Minor mode for an Org buffer backed by Agent Note REST."
  :lighter " AN-Org"
  :keymap agent-note-org-buffer-mode-map
  (if agent-note-org-buffer-mode
      (add-hook 'kill-buffer-query-functions
                #'agent-note-org--confirm-kill nil t)
    (remove-hook 'kill-buffer-query-functions
                 #'agent-note-org--confirm-kill t)))

(defun agent-note-org--confirm-kill ()
  "Ask before discarding unsaved REST-backed Org source."
  (or (not (buffer-modified-p))
      (yes-or-no-p "Discard unsaved Agent Note Org changes? ")))

(defun agent-note-org--write-contents ()
  "Save the current virtual buffer through REST and stop file saving."
  (agent-note-org-save)
  t)

(defun agent-note-org--object (&rest pairs)
  "Return a JSON object from alternating string keys and values in PAIRS."
  (let ((object (make-hash-table :test #'equal)))
    (while pairs
      (let ((key (pop pairs)))
        (unless pairs
          (error "Missing value for JSON key %s" key))
        (puthash key (pop pairs) object)))
    object))

(defun agent-note-org--uuid ()
  "Return a lowercase UUID independent of the user's Org ID settings."
  (let ((org-id-method 'uuid)
        (org-id-prefix nil))
    (downcase (org-id-new))))

(defun agent-note-org--uuid-p (value)
  "Return non-nil when VALUE has canonical UUID shape."
  (and
   (stringp value)
   (string-match-p
    (concat
     "\\`[[:xdigit:]]\\{8\\}-[[:xdigit:]]\\{4\\}-"
     "[[:xdigit:]]\\{4\\}-[[:xdigit:]]\\{4\\}-"
     "[[:xdigit:]]\\{12\\}\\'")
    value)))

(defun agent-note-org--validated-actor-id ()
  "Return the configured actor ID or reject an invalid value."
  (let ((value agent-note-org-actor-id))
    (unless (and (stringp value)
                 (equal value (string-trim value))
                 (not (string-empty-p value))
                 (not (equal value "system")))
      (user-error
       "Agent Note actor ID must be nonblank, trimmed, and not system"))
    value))

(defun agent-note-org--validated-document-path (path)
  "Return PATH or reject a non-portable canonical Org path."
  (unless (and (stringp path)
               (equal path (string-trim path))
               (not (string-empty-p path))
               (not (string-prefix-p "/" path))
               (not (string-suffix-p "/" path))
               (not (string-match-p "//" path))
               (not (string-match-p "\\\\" path))
               (not (string-match-p "\\`[[:alpha:]]:" path))
               (not
                (string-match-p
                 "\\(?:\\`\\|/\\)\\.\\.?\\(?:/\\|\\'\\)" path)))
    (user-error
     (concat
      "Document path must be trimmed, relative, slash-separated, "
      "and contain no . or .. segments")))
  path)

(defun agent-note-org--normalized-base-url (&optional base-url)
  "Validate and normalize BASE-URL or `agent-note-org-base-url'."
  (let ((value (string-trim (or base-url agent-note-org-base-url))))
    (unless (string-match-p "\\`https?://" value)
      (user-error "Agent Note base URL must start with http:// or https://"))
    (replace-regexp-in-string "/+\\'" "" value)))

(defun agent-note-org--request (method path &optional body base-url)
  "Send METHOD to PATH and return its decoded JSON object.
BODY is a hash table or nil.  BASE-URL defaults to the configured server."
  (let* ((base (agent-note-org--normalized-base-url base-url))
         (url-request-method method)
         (url-request-extra-headers
          (append
           (list '("Accept" . "application/json"))
           (when body
             (list '("Content-Type" . "application/json; charset=utf-8")))
           agent-note-org-extra-headers))
         (url-request-data (when body (json-serialize body)))
         (response
          (url-retrieve-synchronously
           (concat base path) t nil agent-note-org-request-timeout)))
    (unless response
      (signal 'agent-note-org-error
              (list (format "No response from %s" base))))
    (unwind-protect
        (with-current-buffer response
          (let ((status url-http-response-status)
                payload)
            (unless (and status url-http-end-of-headers)
              (signal 'agent-note-org-error
                      (list "Malformed HTTP response from Agent Note")))
            (goto-char url-http-end-of-headers)
            (skip-chars-forward "\r\n")
            (setq payload
                  (condition-case parse-error
                      (if (eobp)
                          (make-hash-table :test #'equal)
                        (json-parse-buffer
                         :object-type 'hash-table
                         :array-type 'list
                         :null-object nil
                         :false-object nil))
                    (error
                     (signal
                      'agent-note-org-error
                      (list
                       (format "HTTP %s returned invalid JSON: %s"
                               status (error-message-string parse-error)))))))
            (if (and (>= status 200) (< status 300))
                payload
              (signal 'agent-note-org-http-error (list status payload)))))
      (kill-buffer response))))

(defun agent-note-org--paged-items (path &optional include-archived base-url)
  "Read every page below PATH and return all items.
INCLUDE-ARCHIVED controls the matching REST query.  BASE-URL selects the
server."
  (let ((cursor nil)
        (more t)
        items)
    (while more
      (let* ((query
              (concat
               path
               "?limit=200&include_archived="
               (if include-archived "true" "false")
               (if cursor
                   (concat "&cursor=" (url-hexify-string cursor))
                 "")))
             (page (agent-note-org--request "GET" query nil base-url))
             (page-items (gethash "items" page))
             (next-cursor (gethash "next_cursor" page)))
        (unless (listp page-items)
          (signal 'agent-note-org-error
                  (list "Agent Note returned an invalid paged response")))
        (setq items (nconc items page-items))
        (setq cursor next-cursor)
        (setq more (and (stringp cursor) (not (string-empty-p cursor))))))
    items))

(defun agent-note-org--choose-workspace (base-url)
  "Prompt for an active workspace from BASE-URL and return its JSON object."
  (let ((workspaces
         (agent-note-org--paged-items
          "/api/org/workspaces" nil base-url))
        choices)
    (unless workspaces
      (user-error "Agent Note has no active Org workspaces"))
    (dolist (workspace workspaces)
      (let ((id (gethash "workspace_id" workspace)))
        (push
         (cons
          (format "%s — %s [%s]"
                  (gethash "display_name" workspace)
                  (gethash "slug" workspace)
                  id)
          workspace)
         choices)))
    (cdr
     (assoc
      (completing-read "Agent Note workspace: " choices nil t)
      choices))))

(defun agent-note-org--choose-document (base-url workspace-id)
  "Prompt for a document in WORKSPACE-ID from BASE-URL."
  (let* ((path
          (format "/api/org/workspaces/%s/documents"
                  (url-hexify-string workspace-id)))
         (documents (agent-note-org--paged-items path nil base-url))
         choices)
    (unless documents
      (user-error "This workspace has no active Org documents"))
    (dolist (document documents)
      (let ((id (gethash "id" document)))
        (push
         (cons
          (format "%s — revision %s [%s]"
                  (gethash "path" document)
                  (gethash "revision" document)
                  id)
          document)
         choices)))
    (cdr
     (assoc
      (completing-read "Agent Note document: " choices nil t)
      choices))))

(defun agent-note-org--document-request-path (workspace-id document-id)
  "Return the REST read path for WORKSPACE-ID and DOCUMENT-ID."
  (format "/api/org/documents/%s?workspace_id=%s"
          (url-hexify-string document-id)
          (url-hexify-string workspace-id)))

(defun agent-note-org--find-buffer (base-url document-id)
  "Return an open buffer for DOCUMENT-ID on BASE-URL, if one exists."
  (catch 'found
    (dolist (buffer (buffer-list))
      (with-current-buffer buffer
        (when (and agent-note-org-buffer-mode
                   (equal agent-note-org--base-url base-url)
                   (equal agent-note-org--document-id document-id))
          (throw 'found buffer))))
    nil))

(defun agent-note-org--install-document
    (buffer base-url document &optional modified)
  "Install DOCUMENT in BUFFER and attach Agent Note metadata.
BASE-URL identifies the server.  MODIFIED marks a new unsaved document."
  (with-current-buffer buffer
    (unless (derived-mode-p 'org-mode)
      (org-mode))
    (let ((inhibit-read-only t)
          (buffer-undo-list t))
      (erase-buffer)
      (insert (gethash "source" document))
      (goto-char (point-min)))
    (setq-local agent-note-org--base-url base-url)
    (setq-local agent-note-org--workspace-id
                (gethash "workspace_id" document))
    (setq-local agent-note-org--document-id (gethash "id" document))
    (setq-local agent-note-org--path (gethash "path" document))
    (setq-local agent-note-org--revision (gethash "revision" document))
    (setq-local agent-note-org--pending-save nil)
    (setq-local require-final-newline nil)
    (setq-local write-contents-functions
                '(agent-note-org--write-contents))
    ;; Include modified non-file buffers in manual `save-some-buffers'.
    (setq-local buffer-offer-save 'always)
    (agent-note-org-buffer-mode 1)
    (setq buffer-undo-list nil)
    (set-buffer-modified-p modified)))

(defun agent-note-org-open ()
  "Select and open one canonical Agent Note Org document."
  (interactive)
  (let* ((base-url (agent-note-org--normalized-base-url))
         (workspace (agent-note-org--choose-workspace base-url))
         (workspace-id (gethash "workspace_id" workspace))
         (document (agent-note-org--choose-document base-url workspace-id))
         (document-id (gethash "id" document))
         (existing (agent-note-org--find-buffer base-url document-id)))
    (if existing
        (pop-to-buffer existing)
      (let* ((payload
              (agent-note-org--request
               "GET"
               (agent-note-org--document-request-path
                workspace-id document-id)
               nil base-url))
             (name
              (format "*Agent Note Org: %s [%s]*"
                      (gethash "path" payload)
                      (substring document-id 0 8)))
             (buffer (generate-new-buffer name)))
        (agent-note-org--install-document buffer base-url payload nil)
        (pop-to-buffer buffer)))))

(defun agent-note-org-create (path title)
  "Create an unsaved Org document buffer at PATH with TITLE.
The document is created in Agent Note by the first successful save."
  (interactive
   (let* ((path (read-string "Canonical Org path (for example tasks/inbox.org): "))
          (default-title (file-name-base path)))
     (list path (read-string "Document title: " default-title))))
  (let* ((path (agent-note-org--validated-document-path path))
         (base-url (agent-note-org--normalized-base-url))
         (workspace (agent-note-org--choose-workspace base-url))
         (workspace-id (gethash "workspace_id" workspace))
         (document-id (agent-note-org--uuid))
         (document
          (agent-note-org--object
           "id" document-id
           "workspace_id" workspace-id
           "path" path
           "source" (format "#+TITLE: %s\n\n" title)
           "revision" nil))
         (name
          (format "*Agent Note Org: %s [%s]*"
                  path (substring document-id 0 8)))
         (buffer (generate-new-buffer name)))
    (agent-note-org--install-document buffer base-url document t)
    (pop-to-buffer buffer)
    (message "New Agent Note document; use C-x C-s to create it")))

(defun agent-note-org--ensure-document-buffer ()
  "Reject commands outside an Agent Note Org document buffer."
  (unless (and agent-note-org-buffer-mode
               agent-note-org--base-url
               agent-note-org--workspace-id
               agent-note-org--document-id
               agent-note-org--path)
    (user-error "This is not an Agent Note Org document buffer")))

(defun agent-note-org--buffer-source ()
  "Return the complete current buffer even when Org has narrowed it."
  (save-restriction
    (widen)
    (buffer-substring-no-properties (point-min) (point-max))))

(defun agent-note-org-get-create-id ()
  "Return the current heading ID, creating a UUID ID when necessary.
Unlike `org-id-get-create', this does not register a local file location for
the virtual REST-backed buffer."
  (interactive)
  (agent-note-org--ensure-document-buffer)
  (org-back-to-heading t)
  (let ((id (org-entry-get (point) "ID")))
    (when (and id (not (agent-note-org--uuid-p id)))
      (user-error "Existing ID is not a canonical UUID: %s" id))
    (unless id
      (setq id (agent-note-org--uuid))
      (org-entry-put (point) "ID" id))
    (when (called-interactively-p 'interactive)
      (message "Agent Note work-item ID: %s" id))
    id))

(defun agent-note-org--save-body ()
  "Return a stable request body for the current logical save.
An unchanged retry reuses the prior operation ID."
  (let* ((source (agent-note-org--buffer-source))
         (signature
          (list agent-note-org--base-url
                agent-note-org--workspace-id
                agent-note-org--document-id
                agent-note-org--path
                agent-note-org--revision
                source)))
    (if (equal signature
               (plist-get agent-note-org--pending-save :signature))
        (plist-get agent-note-org--pending-save :body)
      (let ((body
             (agent-note-org--object
              "schema_version" 1
              "actor_id" (agent-note-org--validated-actor-id)
              "operation_id" (agent-note-org--uuid)
              "workspace_id" agent-note-org--workspace-id
              "path" agent-note-org--path
              "source" source
              "expected_revision"
              (if (integerp agent-note-org--revision)
                  agent-note-org--revision
                :null)
              "lease_proofs" (make-hash-table :test #'equal))))
        (setq agent-note-org--pending-save
              (list :signature signature :body body))
        body))))

(defun agent-note-org--error-field (payload field fallback)
  "Read FIELD from error PAYLOAD or return FALLBACK."
  (if (hash-table-p payload)
      (or (gethash field payload) fallback)
    fallback))

(defun agent-note-org-save ()
  "Create or revision-safely update the current Agent Note Org document."
  (interactive)
  (agent-note-org--ensure-document-buffer)
  (unless (buffer-modified-p)
    (user-error "This Agent Note Org buffer has no unsaved changes"))
  (let ((body (agent-note-org--save-body))
        (sent-source nil)
        (path
         (format "/api/org/documents/%s"
                 (url-hexify-string agent-note-org--document-id))))
    (setq sent-source (gethash "source" body))
    (condition-case error-data
        (let* ((result
                (agent-note-org--request
                 "PUT" path body agent-note-org--base-url))
               (revisions (gethash "document_revisions" result))
               (new-revision
                (and (hash-table-p revisions)
                     (gethash agent-note-org--document-id revisions))))
          (unless (integerp new-revision)
            (signal
             'agent-note-org-error
             (list "Save succeeded but returned no document revision")))
          (setq agent-note-org--revision new-revision)
          (setq agent-note-org--pending-save nil)
          (if (equal
               sent-source
               (agent-note-org--buffer-source))
              (progn
                (set-buffer-modified-p nil)
                (message "Saved Agent Note Org document at revision %s"
                         new-revision))
            (set-buffer-modified-p t)
            (message
             (concat
              "Saved revision %s, but the buffer changed during the request; "
              "save again")
             new-revision)))
      (agent-note-org-http-error
       (let* ((status (nth 1 error-data))
              (payload (nth 2 error-data))
              (code
               (agent-note-org--error-field
                payload "code" "unknown_error"))
              (message-text
               (agent-note-org--error-field
                payload "message" "request rejected")))
         (if (equal code "stale_revision")
             (let* ((details (gethash "details" payload))
                    (current
                     (and (hash-table-p details)
                          (gethash "current_revision" details))))
               (user-error
                (concat
                 "Save refused: server revision is %s; your text was kept. "
                 "Use C-c C-v to inspect it")
                (or current "newer")))
           (user-error "Agent Note save failed (HTTP %s, %s): %s"
                       status code message-text))))
      (error
       (signal
        'agent-note-org-error
        (list
         (format
          (concat
           "Save outcome is unknown (%s); keep the buffer unchanged and "
           "retry C-x C-s to reuse the same operation ID")
          (error-message-string error-data))))))))

(defun agent-note-org-reload ()
  "Reload the current document, confirming before discarding local edits."
  (interactive)
  (agent-note-org--ensure-document-buffer)
  (when (and (buffer-modified-p)
             (not (yes-or-no-p
                   "Discard local edits and reload the server version? ")))
    (user-error "Reload cancelled"))
  (let ((before-tick (buffer-chars-modified-tick))
        (payload
         (agent-note-org--request
          "GET"
          (agent-note-org--document-request-path
           agent-note-org--workspace-id agent-note-org--document-id)
          nil agent-note-org--base-url))
        (buffer (current-buffer))
        (base-url agent-note-org--base-url))
    (unless (= before-tick (buffer-chars-modified-tick))
      (user-error
       "Buffer changed while reloading; local text was kept, retry explicitly"))
    (agent-note-org--install-document buffer base-url payload nil)
    (message "Reloaded Agent Note Org revision %s"
             agent-note-org--revision)))

(defun agent-note-org-show-server-version ()
  "Open the current server version in a separate read-only Org buffer."
  (interactive)
  (agent-note-org--ensure-document-buffer)
  (let* ((payload
          (agent-note-org--request
           "GET"
           (agent-note-org--document-request-path
            agent-note-org--workspace-id agent-note-org--document-id)
           nil agent-note-org--base-url))
         (revision (gethash "revision" payload))
         (name
          (format "*Agent Note server: %s r%s*"
                  agent-note-org--path revision))
         (buffer (generate-new-buffer name)))
    (with-current-buffer buffer
      (let ((inhibit-read-only t)
            (buffer-undo-list t))
        (read-only-mode -1)
        (erase-buffer)
        (insert (gethash "source" payload))
        (goto-char (point-min)))
      (org-mode)
      (read-only-mode 1)
      (set-buffer-modified-p nil))
    (pop-to-buffer buffer)
    (message "Use M-x ediff-buffers to compare local and server versions")))

(provide 'agent-note-org)

;;; agent-note-org.el ends here
```

### Why the package uses virtual buffers

The buffer has no fake local filename. A buffer-local
`write-contents-functions` handler completes the save through REST before Emacs
tries to ask for a filename. The major mode remains `org-mode`, and standard
`C-x C-s`, `save-some-buffers`, and exit-time save prompts use the REST path.
The package disables automatic final-newline insertion so a save does not alter
canonical source behind the user's back.

Giving the buffer a pretend `buffer-file-name` would accidentally involve file
backups, auto-save, `recentf`, project detection, and other filesystem behavior.
This first package also does not add these buffers to `org-agenda-files`.
Server-backed agenda integration should query `GET /api/org/agenda` instead of
pretending the canonical documents are local files.

There is no local auto-save or backup file in this reference implementation.
Emacs offers modified buffers for REST save during normal exit and asks before
killing one, but an editor or machine crash can still lose unsaved text. A
production package should add an explicit recovery cache without treating that
cache as a second canonical source.

Other file-oriented Org features also do not acquire a useful local path.
Relative file links, `#+INCLUDE`, attachments whose location derives from the
visited filename, project commands, and similar features need an explicit
server-backed or cache-backed design before they can be considered supported.

### Why saves retain a pending operation

The server makes mutation replay idempotent by `operation_id`. A timeout can
happen after a request was committed but before Emacs received the response.
The package retains the exact request while its document path, source, and
expected revision remain unchanged. Pressing `C-x C-s` again therefore asks for
the original result rather than applying a second logical mutation.

Changing the buffer produces a new request and operation ID. A successful save
updates the buffer-local revision and clears the pending operation. If Emacs
hooks or timers change the buffer while the synchronous request is in flight,
the returned revision is adopted but the new text remains marked as modified so
the user can save it separately.

## 5. Load and configure the package

Add the package directory and configuration to `init.el`:

```emacs-lisp
(add-to-list 'load-path (expand-file-name "lisp" user-emacs-directory))
(require 'agent-note-org)

(setq agent-note-org-base-url "http://127.0.0.1:6222")
(setq agent-note-org-actor-id
      (format "emacs:%s" (user-login-name)))
```

If an authenticating reverse proxy requires headers, set
`agent-note-org-extra-headers` according to that proxy's contract. Avoid storing
a real shared or production credential directly in a version-controlled
`init.el`; retrieve it from `auth-source` or another local secret store.

Evaluate the forms or restart Emacs.

This guide assumes the workspace already exists because workspace creation
requires an explicit workflow policy. On a fresh Agent Note installation, open
`http://127.0.0.1:6222/api/docs` and use `POST /api/org/workspaces` to create a
workspace with the intended policy before running the package commands. See the
repository `README.md` setup and run sections for complete application bootstrap
instructions.

## 6. Use Agent Note Org from Emacs

### Open an existing document

Run:

```text
M-x agent-note-org-open
```

Select a workspace, then a document. The returned source opens in `org-mode`.
The buffer stores the base URL, workspace ID, document ID, canonical path, and
revision as buffer-local state.

Selecting a document that is already open switches to its existing buffer and
does not fetch it again. Use `C-c C-r` when an explicit refresh is intended.

Edit normally and press `C-x C-s`. A successful save reports the new revision
and marks the buffer unmodified.

### Create a document

Run:

```text
M-x agent-note-org-create
```

Enter a portable path such as `projects/compiler.org`, a title, and the target
workspace. The package generates a document UUID locally. The buffer is not
created on the server until the first `C-x C-s` succeeds.

Document paths are canonical names inside a workspace, not local filesystem
paths. Use relative slash-separated paths. Do not use an absolute path or `..`.

### Reload deliberately

Run `M-x agent-note-org-reload` or press `C-c C-r`. If the buffer has local
changes, Emacs asks before discarding them. Reloading replaces the buffer with
the current canonical source and revision.

### Resolve a stale revision

When saving reports `stale_revision`:

1. Keep the dirty local buffer open.
2. Press `C-c C-v` to fetch the current server source into a separate read-only
   buffer.
3. Run `M-x ediff-buffers` and select the local and server buffers.
4. Copy the intended local changes or completed merge into a temporary buffer.
5. Reload the original buffer to adopt the current source and revision.
6. Apply the reviewed merge to that reloaded buffer and save.

The package intentionally does not replace the local revision with the
`current_revision` value from the error. Doing so would turn the next save into
a blind overwrite without first reading the source associated with that
revision.

Other HTTP `409` errors are also meaningful. For example, `stale_lease` rejects
a raw document edit when an affected work item is owned by an active agent lease
and the request does not carry the exact proof set. This package intentionally
sends no lease fencing tokens, displays the server's error, and keeps local
edits unsaved.

## 7. Write Org source that Agent Note can orchestrate

Agent Note preserves canonical Org source, including ordinary content it does
not project. A heading becomes an orchestrated work item when its direct
property drawer contains `AGENT_NOTE_TYPE`.

An orchestrated heading must have exactly one UUID `ID` and exactly one
`AGENT_NOTE_TYPE`:

```org
#+TITLE: Engineering work
#+TODO: BACKLOG READY RUNNING BLOCKED REVIEW | DONE FAILED CANCELLED

* READY [#A] Ship the Emacs integration :editor:org:
SCHEDULED: <2026-08-12 Wed 09:30>
DEADLINE: <2026-08-14 Fri>
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:ASSIGNEE: human:gao
:REQUIRES_REVIEW: true
:END:
Document the REST-backed editing workflow.
```

Supported work-item types are:

```text
project epic issue task subtask review approval incident milestone
```

Important source fields include:

| Org source | Agent Note meaning |
| --- | --- |
| Heading TODO keyword | Workflow state; it must follow the workspace policy |
| Heading priority and tags | Projected priority and tags |
| `ID` | Stable work-item UUID |
| `AGENT_NOTE_TYPE` | Work-item type and projection marker |
| `ASSIGNEE` | Current assignee |
| `DEPENDS_ON` | Space-separated work-item UUIDs |
| `REQUIRES_REVIEW` | Exactly `true` or `false` |
| `SCHEDULED` and `DEADLINE` | Workspace-time scheduling data |
| `[[agent-note:<purpose>:<note-uuid>][description]]` | Weak link to a Markdown note |

The valid states, transitions, types, review requirements, tags, retry limits,
and concurrency rules come from the selected workspace policy. Do not hard-code
the example `#+TODO` sequence for every workspace. Inspect the workspace through
`GET /api/org/workspaces/{workspace_id}` or the Swagger UI before authoring a
new workflow document.

Raw document writes are deliberately narrower than the complete Org mutation
API. They may add new valid work items and can update body text, title, priority,
assignee, schedule, deadline, dependencies, note links, and policy-valid state
transitions. They cannot implicitly delete an existing projected work item or
change an existing item's type, tags, or review requirement. Agent Note returns
`unsupported_semantic_edit` and applies nothing when a raw save attempts one of
those changes. Do not invent a client-side workaround; consult the current
OpenAPI surface before adding a purpose-built mutation, because the initial API
does not provide a dedicated operation for every prohibited raw change.

With point on a heading, create a stable UUID using the package command:

```text
M-x agent-note-org-get-create-id
```

Then add `AGENT_NOTE_TYPE` and any optional Agent Note properties to the same
heading's direct property drawer. The server validates the complete source on
save and changes nothing if validation fails.

Do not use `org-id-get-create` for this reference package's virtual buffers. That
Org command also tries to register a local file location, but these buffers do
not represent local files. `agent-note-org-get-create-id` adds the canonical
UUID property without inventing a filesystem location.

## 8. Verify the package

### Byte-compile it

Run:

```sh
emacs -Q --batch \
  -L "$HOME/.emacs.d/lisp" \
  -f batch-byte-compile "$HOME/.emacs.d/lisp/agent-note-org.el"
```

Expected result: exit status zero and an
`~/.emacs.d/lisp/agent-note-org.elc` file.

### Perform a safe manual acceptance test

Use a disposable workspace or document:

1. Start Agent Note and confirm the workspace-list `curl` request succeeds.
2. Run `M-x agent-note-org-create` and create `emacs/acceptance.org`.
3. Add one valid orchestrated heading and run
   `M-x agent-note-org-get-create-id` on it.
4. Add `AGENT_NOTE_TYPE` with a type allowed by the workspace policy.
5. Press `C-x C-s`; confirm Emacs reports revision 1.
6. Change the document and save again; confirm the revision increases.
7. Open the same document through Swagger or another client and update it.
8. Make a different Emacs edit and save; confirm Emacs reports
   `stale_revision` and retains the local text.
9. Press `C-c C-v`; confirm the separate buffer contains the newer server
   source.
10. Reload, merge deliberately, save, and confirm the final canonical source
    through `GET /api/org/documents/{document_id}?workspace_id={workspace_id}`.

Also test an unavailable server: make an edit, stop Agent Note, and press
`C-x C-s`. Restart Agent Note and retry without changing the buffer. This checks
that the package retains and reuses the pending operation. Proving the harder
"server committed, response was lost" case requires a proxy or test server that
can drop the response after forwarding the successful mutation.

## 9. Production package extensions

Keep the first package focused until document editing is reliable. Useful later
extensions are:

- asynchronous requests with `url-retrieve` so a slow network never blocks the
  Emacs UI;
- an `auth-source` callback for reverse-proxy credentials;
- workspace-policy-aware TODO keyword setup for new documents;
- a tabulated workspace/document browser;
- ERT tests with a local mock HTTP server;
- `GET /api/org/queue` and `GET /api/org/agenda` views; and
- explicit commands for assignment, transitions, dependencies, and reviews.

Do not implement local filesystem watching or background bidirectional merge as
an incidental extension. Those features introduce a second synchronization
model and need their own conflict and ownership design.

For the complete current REST surface and request schemas, run Agent Note and
open `http://127.0.0.1:6222/api/docs`, or read
`http://127.0.0.1:6222/api/openapi.json`.

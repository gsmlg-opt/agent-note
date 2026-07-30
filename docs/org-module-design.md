# Agent Note Org Orchestration System

## Design Purpose and Expected Outcomes

## 1. Background

Agent Note currently serves as a persistent knowledge system for AI agents, with Markdown notes used to store research, design documents, implementation records, summaries, memories, and other narrative information.

Markdown is well suited to knowledge preservation, but it is not sufficient as an execution model. It does not provide strong native semantics for task state, scheduling, dependencies, assignment, work queues, review flows, or long-running agent coordination.

The Org Orchestration System introduces a dedicated Org-based subsystem within Agent Note. Its purpose is not to replace Markdown notes or add another generic document format. Its purpose is to provide a structured execution plane for agents.

The intended division is:

* Markdown notes represent knowledge, context, reasoning, and results.
* Org workspaces represent plans, issues, tasks, schedules, workflow state, and execution coordination.

Together, these two systems allow Agent Note to evolve from a note storage service into a complete knowledge and orchestration platform for AI agents.

---

## 2. Design Purpose

### 2.1 Establish a Persistent Execution Plane

The primary purpose of the Org subsystem is to provide a durable execution model for agents.

Agent tasks must survive process restarts, agent replacement, network interruptions, context-window expiration, and long-running workflows. Task state must not depend on a single conversation or a temporary agent process.

The Org subsystem should preserve:

* What work exists
* Why the work exists
* Who or what is responsible for it
* Its current lifecycle state
* Its dependencies
* Its planned execution time
* Its deadline
* Its execution history
* Its relationship to knowledge stored in Markdown notes

This creates a stable coordination layer independent of any individual agent runtime.

### 2.2 Treat Org as an Orchestration Model

Org should not be treated merely as another text format.

The subsystem should interpret Org documents as structured orchestration resources containing:

* Projects
* Epics
* Issues
* Tasks
* Subtasks
* Reviews
* Approvals
* Incidents
* Milestones
* Schedules
* Dependencies
* Workflow states

The hierarchical structure of Org naturally represents decomposition. A parent issue may contain tasks, and tasks may contain subtasks. Org properties, states, tags, timestamps, and stable identifiers provide the metadata required for orchestration.

The source remains standard Org text, but the system should understand and manage its execution semantics.

### 2.3 Preserve Org Compatibility

The system should remain compatible with normal Org Mode workflows.

Org documents must remain valid, readable, exportable, editable Org files. Users should be able to access and edit them through Emacs without depending on a proprietary representation.

The service may maintain indexes, task records, dependency graphs, execution events, and search metadata, but these should remain derived or associated structures rather than replacements for the original Org content.

This ensures:

* Compatibility with Emacs and Org Mode
* Long-term data portability
* Human-readable storage
* Independence from the Agent Note implementation
* Recovery through standard text exports
* Reduced vendor and architecture lock-in

### 2.4 Support Multiple Independent Workspaces

The Org subsystem should support multiple workspaces as first-class orchestration boundaries.

A workspace represents an independent organizational and workflow domain. Example workspaces may include:

* Personal
* Engineering
* Research
* Operations
* Agent Runtime
* Individual software projects
* Team-specific environments

Each workspace may define its own:

* Task types
* Workflow states
* State transition rules
* Scheduling policy
* Agent assignment policy
* Review requirements
* Retry behavior
* Concurrency limits
* Archive rules
* Tag conventions
* Access policy

Workspaces allow Agent Note to support different execution models without forcing all tasks into one global workflow.

### 2.5 Separate Knowledge from Execution

The design should establish a clear semantic boundary between Markdown notes and Org orchestration data.

Markdown notes should continue to store:

* Research
* Architecture documents
* Design decisions
* Investigation results
* Implementation notes
* Session summaries
* Long-term memory
* Reports
* Postmortems

Org should store:

* Work items
* Execution plans
* Workflow states
* Dependencies
* Schedules
* Deadlines
* Assignment
* Review status
* Progress
* Execution coordination

The two systems should be connected through explicit references.

A task may link to the Markdown design document that explains it. A Markdown note may link to the issue or task responsible for implementing its conclusions.

This separation avoids forcing narrative knowledge into task structures or using unstructured notes as a workflow engine.

### 2.6 Provide Agent-Native Orchestration

The Org subsystem should be designed as a first-class interface for AI agents.

Agents should be able to:

* Discover available work
* Query task queues
* Read issue and task context
* Resolve dependencies
* Claim tasks
* Update progress
* Report blockers
* Request review
* Complete or fail tasks
* Create follow-up work
* Link execution results to Markdown notes
* Retrieve scheduled and deadline-driven work

Agent interaction should be based on orchestration semantics rather than arbitrary text replacement.

The system should expose actions such as claiming a task, transitioning state, adding a dependency, or submitting a review. Agents should not normally need to rewrite entire Org documents to perform routine operations.

### 2.7 Prevent Concurrent Execution Conflicts

A core design purpose is to coordinate multiple agents safely.

The system must prevent multiple agents from unintentionally executing the same task simultaneously. It should provide task ownership or lease semantics so that work can be claimed for a limited period.

The orchestration layer should support:

* Exclusive task claims
* Lease expiration
* Heartbeats
* Task release
* Recovery after agent failure
* Reassignment
* Concurrent update detection
* Explicit conflict handling

This allows multiple agent runtimes to operate against the same workspace without relying on informal coordination.

### 2.8 Support Explicit Workflow State Machines

Task state should not be treated as arbitrary text.

Each workspace should define valid lifecycle states and transitions. A typical engineering workflow may include:

* Backlog
* Ready
* Running
* Blocked
* Review
* Done
* Failed
* Cancelled

The system should enforce valid transitions and reject inconsistent state changes.

For example:

* A task should not become Ready while required dependencies remain incomplete.
* A task requiring review should not transition directly from Running to Done.
* A task should not be completed by an agent that does not hold the active lease.
* A failed task may require an explicit retry or cancellation decision.

This turns Org TODO states into a reliable orchestration state machine.

### 2.9 Represent Dependencies Explicitly

The system should support explicit dependency relationships between work items.

Dependencies should be based on stable task identifiers rather than heading text or document position.

The orchestration layer should understand:

* A task depends on another task
* A task blocks another task
* A task is part of a larger issue
* A task requires a review or approval
* A milestone depends on a set of tasks
* Work may span multiple Org documents
* Work may span multiple workspaces where policy permits

Dependency information should be queryable and usable by the scheduler.

Only executable work whose dependencies are satisfied should enter an agent-ready queue.

### 2.10 Maintain an Execution History

The current Org document represents the current intended state, but it is not sufficient as an execution audit trail.

The system should record append-only execution events describing important changes, including:

* Creation
* Assignment
* Claim
* Start
* Progress update
* Block
* Unblock
* Review request
* Approval
* Rejection
* Completion
* Failure
* Retry
* Cancellation
* Release
* Lease expiration

This history should make it possible to understand:

* Which agent performed an action
* When it happened
* What state changed
* Why the task was blocked or failed
* How many attempts occurred
* Which execution produced a result
* Which notes or artifacts were created

The event history should support auditing, debugging, reporting, and future workflow optimization.

### 2.11 Provide Agenda and Queue Semantics

The system should provide more than document browsing.

It should expose work through operational views such as:

* Tasks ready for execution
* Tasks assigned to a particular agent
* Blocked tasks
* Tasks awaiting review
* Scheduled tasks
* Upcoming deadlines
* Failed tasks awaiting retry
* Tasks with expired leases
* High-priority issues
* Workspace-specific agendas
* Cross-workspace agendas

These views should be derived from structured indexes rather than requiring every client to scan and parse all Org documents.

This enables efficient agent scheduling and human oversight.

### 2.12 Integrate Through the Existing Agent Note MCP Server

The Org subsystem should be exposed through the existing Agent Note MCP service.

Agent Note should remain the unified data interface for agents, with separate semantic tool families for:

* Markdown knowledge
* Org orchestration

The MCP interface should clearly distinguish between reading knowledge and performing execution operations.

The Org tools should cover:

* Workspace discovery
* Issue and task creation
* Work queue queries
* Task claim and release
* State transitions
* Scheduling
* Dependency management
* Progress reporting
* Review workflows
* Agenda queries
* Linking tasks to Markdown notes

This allows agents to use one trusted service for both knowledge and execution without collapsing their data models.

### 2.13 Maintain Human Control

Although the system is agent-native, it must remain human-operable.

Humans should be able to:

* Inspect workspaces
* Edit Org documents through Emacs
* Review task history
* Override assignments
* Resolve conflicts
* Approve or reject work
* Move tasks between states
* Reassign failed work
* Change workspace policies
* Export all Org data

The system should enhance Org Mode rather than replace it with an opaque automation layer.

### 2.14 Support Future Scheduling and Automation

The initial design should leave room for a future scheduler that can select and dispatch work automatically.

The scheduler may eventually consider:

* Task state
* Dependency completion
* Schedule and deadline
* Priority
* Agent capabilities
* Workspace policy
* Concurrency limits
* Resource availability
* Retry policy
* Historical agent performance
* Review requirements

The first version does not need to implement a fully autonomous scheduler, but the data and workflow model must not prevent one from being added later.

---

## 3. Expected System Behavior

### 3.1 Workspace Isolation

Each Org workspace should behave as an independent orchestration domain.

Documents, paths, configurations, agendas, queues, and permissions should be scoped to a workspace. Cross-workspace queries should be explicit rather than accidental.

A workspace should be independently exportable, archivable, and manageable.

### 3.2 Stable Work Item Identity

Every orchestrated issue, task, review, or milestone should have a stable identifier.

Its identity should remain unchanged when:

* The heading is renamed
* The task is moved within a document
* The task is moved to another document
* The document path changes
* The document moves between workspaces
* The surrounding hierarchy changes

Stable identity is required for dependencies, links, event history, agent references, and external integrations.

### 3.3 Revision-Safe Updates

Changes from Emacs, Web clients, MCP agents, and imports should not silently overwrite one another.

The system should detect when a document or task was modified after a client read it. Conflicting updates should be rejected or explicitly reconciled.

This is particularly important when a human edits a task in Emacs while an agent is simultaneously executing it.

### 3.4 Structured Agent Operations

Agents should normally interact with tasks through domain operations.

Examples include:

* Create an issue
* Add a subtask
* Claim a task
* Mark a task blocked
* Add a dependency
* Schedule work
* Request review
* Submit a result
* Complete a task
* Create a follow-up task

Direct whole-document replacement should exist only as a lower-level administrative capability.

### 3.5 Linked Knowledge Context

Every task should be able to reference relevant Markdown notes.

When an agent claims work, the system should be able to provide:

* Task description
* Parent issue
* Dependencies
* Related Markdown notes
* Prior execution events
* Relevant results
* Workspace rules
* Review requirements

This minimizes repeated discovery work and gives the agent a complete execution context.

### 3.6 Recoverable Agent Execution

If an agent crashes or disappears, its task must not remain permanently locked.

The system should detect expired claims and make the task available for recovery according to workspace policy.

The recovered task should retain:

* Previous attempt history
* Partial progress
* Failure or timeout information
* Linked artifacts
* Existing notes
* Retry count

A replacement agent should be able to continue from the preserved state.

### 3.7 Auditable Completion

A task should not simply disappear when completed.

Completion should preserve:

* The final state
* Completion time
* Responsible agent or human
* Execution attempt
* Result summary
* Related Markdown note
* Produced artifacts
* Review outcome
* Relevant event history

Completed work may later be archived, but it should remain queryable.

---

## 4. Expected Outcomes

### 4.1 Agent Note Becomes a Knowledge and Execution Platform

The most important outcome is that Agent Note evolves from a Markdown note storage system into a two-plane platform:

* A knowledge plane for persistent information
* An execution plane for structured work

This gives agents both durable memory and durable operational state.

### 4.2 Long-Running Agent Workflows Become Reliable

Agent work should no longer depend on one prompt, one chat session, or one agent process.

A workflow may continue across:

* Multiple agents
* Multiple execution attempts
* Multiple machines
* Multiple days
* Human review cycles
* Agent restarts
* Model changes

The system should preserve continuity throughout the entire lifecycle.

### 4.3 Human and Agent Work Use the Same Source of Truth

Humans using Emacs and agents using MCP should operate on the same Org workspaces.

There should be no separate hidden agent task database that diverges from the human-visible workflow.

Org documents provide the human-readable representation, while Agent Note provides indexing, validation, orchestration, and access control.

### 4.4 Multiple Agents Can Cooperate Safely

The system should enable multiple agents to work concurrently without duplicating tasks or overwriting each other.

Agents should be able to:

* Discover separate ready tasks
* Claim work safely
* Observe dependencies
* Coordinate through task state
* Hand work to review agents
* Recover failed work
* Create follow-up tasks

This provides the foundation for controlled multi-agent execution.

### 4.5 Work Becomes Observable

Users should be able to answer operational questions such as:

* What is currently running?
* Which agent owns each task?
* What is blocked?
* What is waiting for review?
* What failed recently?
* Which deadlines are approaching?
* Which projects have stalled?
* What work is ready next?
* What did an agent produce?
* Why was a task retried?

The Org subsystem should make agent execution visible and inspectable.

### 4.6 Knowledge and Work Remain Connected

Design documents should not become disconnected from implementation tasks.

The system should preserve explicit relationships between:

* Research and resulting issues
* Architecture documents and implementation tasks
* Tasks and execution reports
* Incidents and postmortems
* Reviews and revised designs
* Completed work and generated knowledge

This creates a traceable path from reasoning to execution and from execution back to knowledge.

### 4.7 Workspace-Specific Workflows Become Possible

Different domains should be able to use different operational models.

For example:

* Engineering may require review before completion.
* Research may use investigation and synthesis stages.
* Operations may prioritize incidents and deadlines.
* Personal work may use a simple TODO and DONE flow.
* Agent runtime workspaces may use queue, claim, run, retry, and failure states.

The system should support these differences without duplicating the entire platform.

### 4.8 Agent Orchestration Remains Portable

Because standard Org text remains the primary representation, users should be able to export and retain their work independently of Agent Note.

The orchestration service may add indexes, events, leases, and metadata, but the essential plans and tasks should remain readable in Org Mode.

This protects long-term ownership of the data.

### 4.9 Future Autonomous Scheduling Becomes Feasible

Once tasks, states, dependencies, agents, leases, and events are represented consistently, Agent Note can later support automatic dispatch.

A scheduler could continuously identify executable work and assign it to capable agents while respecting policy and concurrency constraints.

The initial Org subsystem should establish the foundation for this capability without requiring full autonomy in the first release.

---

## 5. Product Positioning

The Org subsystem should be positioned as:

> A multi-workspace, Org-native orchestration system for managing issues, tasks, dependencies, schedules, reviews, and persistent AI agent execution.

Within Agent Note:

> Markdown stores what agents know. Org manages what agents do.

This distinction should guide all future design decisions.

The system is not intended to become a generic project-management clone or a replacement for Org Mode. Its purpose is to combine Org’s human-readable workflow model with Agent Note’s persistent storage, indexing, MCP access, and agent-oriented execution controls.

---

## 6. Success Criteria

The design should be considered successful when the following outcomes are achieved:

* A user can create multiple isolated Org workspaces.
* Standard Org documents can be stored, retrieved, edited, and exported.
* Tasks and issues have stable identities.
* Agents can discover and claim ready work through MCP.
* Duplicate execution is prevented through lease or ownership semantics.
* State transitions are validated according to workspace policy.
* Dependencies determine whether tasks are executable.
* Humans can edit the same work through Emacs.
* Concurrent modifications are detected instead of silently overwritten.
* Every meaningful execution action is auditable.
* Tasks can link to Markdown notes for context and results.
* Failed or interrupted work can be recovered.
* Agenda and queue views can be generated without scanning every document on the client.
* Existing Markdown note behavior remains unchanged.
* Agent Note exposes both knowledge and orchestration through one unified service boundary.
* The architecture can later support automatic scheduling and multi-agent dispatch.

---

## 7. Final Design Principle

The central design principle is:

> Org is the durable execution model, Markdown is the durable knowledge model, and Agent Note connects them into one agent-native operating system for knowledge and work.

The Org subsystem should therefore be designed around execution semantics, workflow integrity, concurrency safety, human readability, and long-term portability—not merely around storing additional text files.

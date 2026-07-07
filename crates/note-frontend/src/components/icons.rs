//! Small inline (Lucide-style) SVG icons for icon buttons. Stroke uses `currentColor` so they
//! take the button's text color.
use yew::prelude::*;

fn svg(children: Html) -> Html {
    html! {
        <svg class="icon" width="18" height="18" viewBox="0 0 24 24" fill="none"
            stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
            aria-hidden="true">
            { children }
        </svg>
    }
}

pub fn eye() -> Html {
    svg(html! {
        <>
            <path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7z" />
            <circle cx="12" cy="12" r="3" />
        </>
    })
}

pub fn pencil() -> Html {
    svg(html! {
        <>
            <path d="M12 20h9" />
            <path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z" />
        </>
    })
}

pub fn trash() -> Html {
    svg(html! {
        <>
            <polyline points="3 6 5 6 21 6" />
            <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
        </>
    })
}

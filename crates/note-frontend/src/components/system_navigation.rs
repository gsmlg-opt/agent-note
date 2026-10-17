use yew::prelude::*;
use yew_router::prelude::*;

use crate::routes::SystemSection;

#[derive(Properties, PartialEq)]
pub struct SystemNavigationProps {
    pub section: SystemSection,
}

#[function_component(SystemNavigation)]
pub fn system_navigation(props: &SystemNavigationProps) -> Html {
    let navigator = use_navigator();

    html! {
        <nav class="system-navigation" aria-label="System settings">
            <p class="system-navigation-title">{ "Settings" }</p>
            <ul class="system-navigation-list">
                { for SystemSection::ALL.into_iter().map(|section| {
                    let destination = section.route();
                    let href = destination.to_path();
                    let navigator = navigator.clone();
                    let active = props.section == section;
                    let onclick = Callback::from(move |event: MouseEvent| {
                        if event.button() == 0
                            && !event.alt_key()
                            && !event.ctrl_key()
                            && !event.meta_key()
                            && !event.shift_key()
                        {
                            if let Some(navigator) = &navigator {
                                event.prevent_default();
                                navigator.push(&destination);
                            }
                        }
                    });

                    html! {
                        <li key={href.clone()}>
                            <a
                                {href}
                                {onclick}
                                class={classes!("system-navigation-link", active.then_some("is-active"))}
                                aria-current={active.then_some("page")}
                            >
                                { section.label() }
                            </a>
                        </li>
                    }
                }) }
            </ul>
        </nav>
    }
}

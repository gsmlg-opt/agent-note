use web_sys::HtmlInputElement;
use yew::prelude::*;
use yew_duskmoon::{Button, Card};

use crate::state::SearchResultSummary;

#[derive(Properties, PartialEq)]
pub struct VectorSearchProps {
    pub results: Vec<SearchResultSummary>,
    pub loading: bool,
    /// Emits the query string when the user runs a search.
    pub on_query: Callback<String>,
}

#[function_component(VectorSearch)]
pub fn vector_search(props: &VectorSearchProps) -> Html {
    let query = use_state(String::new);

    let on_input = {
        let query = query.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            query.set(input.value());
        })
    };

    let on_search = {
        let on_query = props.on_query.clone();
        let query = query.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            on_query.emit((*query).clone());
        })
    };

    html! {
        <Card title={Some(html! { "Search" })} classes={classes!("vector-search")}>
            <form class="search-bar" onsubmit={on_search}>
                <input
                    class="input input-primary"
                    type="text"
                    placeholder="Search notes"
                    value={(*query).clone()}
                    oninput={on_input}
                />
                <Button variant={Some("primary".to_string())}>{ "Search" }</Button>
            </form>

            if props.loading {
                <p class="loading">{ "Searching…" }</p>
            }

            <ul class="results">
                { for props.results.iter().map(|r| {
                    html! {
                        <li class="result" key={r.id.clone()}>
                            <div class="result-title">{ r.title.clone() }</div>
                            <div class="result-id">{ r.id.clone() }</div>
                            // Score is an RRF rank-fusion score, not a raw
                            // similarity/distance — label it accordingly
                            // (docs/design.md §7).
                            <div class="result-score">
                                { format!("fused score: {:.4}", r.score) }
                            </div>
                        </li>
                    }
                }) }
            </ul>
        </Card>
    }
}

use crate::components::search::{SearchMode, SearchPropertiesMode};
use crate::utils::search::*;
use gloo_utils::format::JsValueSerdeExt;
use patternfly_yew::prelude::*;
use std::fmt::Debug;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use wasm_bindgen::JsValue;
use yew::prelude::*;

pub const DEFAULT_PAGE_SIZE: usize = 10;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SearchViewState<T> {
    pub search: T,
    pub pagination: PaginationControl,
}

impl<T> Deref for SearchViewState<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.search
    }
}

impl<T> DerefMut for SearchViewState<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.search
    }
}

#[hook]
pub fn use_search_view_state<T, F>(
    props_query: Option<String>,
    total: Option<usize>,
    f: F,
) -> (UseStateHandle<T>, UsePagination)
where
    T: for<'de> serde::Deserialize<'de> + serde::Serialize + Clone + Default + Debug + PartialEq + 'static,
    F: FnOnce(String) -> T,
{
    // the active query
    let state = use_memo(
        |()| {
            // initialize with the state from history, properties, or with a reasonable default
            gloo_utils::history()
                .state()
                .ok()
                .and_then(|state| {
                    let deser = state.into_serde::<SearchViewState<T>>();
                    log::debug!("Deserialized: {deser:?}");
                    deser.ok()
                })
                .or_else(|| {
                    props_query.and_then(|s| {
                        log::debug!("Initial: {s}");
                        match s.is_empty() {
                            true => None,
                            false => Some(SearchViewState {
                                search: f(s),
                                pagination: PaginationControl {
                                    per_page: DEFAULT_PAGE_SIZE,
                                    ..Default::default()
                                },
                            }),
                        }
                    })
                })
                .unwrap_or_default()
        },
        (),
    );

    let search_params = use_state_eq(|| state.search.clone());
    let pagination = use_pagination(total, || state.pagination.clone());

    // when the search params or pagination state changes, store in history API
    use_effect_with_deps(
        |(search_params, pagination)| {
            // store changes to the state in the current history
            if let Ok(data) = JsValue::from_serde(&SearchViewState {
                search: search_params,
                pagination: pagination.clone(),
            }) {
                let _ = gloo_utils::history().replace_state(&data, "");
            }
        },
        ((*search_params).clone(), (pagination.state.control).clone()),
    );

    (search_params, pagination)
}

pub struct UseStandardSearch<T> {
    pub search_params: UseStateHandle<SearchMode<T>>,
    pub pagination: UsePagination,
    pub filter_input_state: Rc<InputState>,
    pub onclear: Callback<()>,
    pub onset: Callback<()>,
    pub ontogglesimple: Callback<bool>,
    pub text: UseStateHandle<String>,
}

#[hook]
pub fn use_standard_search<T, S>(
    mode: SearchPropertiesMode,
    total: Option<usize>,
    context: Rc<T::Context>,
) -> UseStandardSearch<T>
where
    T: for<'de> serde::Deserialize<'de>
        + serde::Serialize
        + Clone
        + Default
        + Debug
        + PartialEq
        + ToFilterExpression
        + SimpleProperties
        + 'static,
    T::Context: PartialEq,
    S: sikula::prelude::Search,
{
    let (search_params, pagination) =
        use_search_view_state::<SearchMode<T>, _>(mode.props_query(), total, SearchMode::Complex);

    // the current value in the text input field
    let text = use_state_eq(|| match &*search_params {
        SearchMode::Complex(s) => s.to_string(),
        SearchMode::Simple(s) => s.terms().join(" "),
    });

    // parse filter
    let filter_input_state = use_memo(
        |(simple, text)| match simple {
            true => InputState::Default,
            false => match S::parse(text) {
                Ok(_) => InputState::Default,
                Err(err) => {
                    log::info!("Failed to parse: {err}");
                    InputState::Error
                }
            },
        },
        ((*search_params).is_simple(), (*text).clone()),
    );

    // clear search, keep mode
    let onclear = use_callback(
        |_, (text, search_params)| {
            text.set(String::new());
            // trigger empty search
            match **search_params {
                SearchMode::Complex(_) => search_params.set(SearchMode::Complex(String::new())),
                SearchMode::Simple(_) => search_params.set(SearchMode::Simple(Default::default())),
            }
        },
        (text.clone(), search_params.clone()),
    );

    // apply text field to search
    let onset = use_callback(
        |(), (text, search_params)| match (**search_params).clone() {
            SearchMode::Complex(_) => {
                search_params.set(SearchMode::Complex((**text).clone()));
            }
            SearchMode::Simple(mut s) => {
                *s.terms_mut() = text.split(' ').map(|s| s.to_string()).collect();
                search_params.set(SearchMode::Simple(s));
            }
        },
        (text.clone(), search_params.clone()),
    );

    // when the search mode changes, and is "provided", refresh the simple terms
    use_effect_with_deps(
        |(mode, search_params)| {
            if let SearchPropertiesMode::Provided { terms } = mode {
                let terms = terms.split(' ').map(|s| s.to_string()).collect();
                search_params.set(search_params.set_simple_terms(terms));
            }
        },
        (mode.clone(), search_params.clone()),
    );

    let ontogglesimple = use_callback(
        |state, (text, context, search_params)| match state {
            false => {
                let q = (*search_params).as_str(context).to_string();
                search_params.set(SearchMode::Complex(q.clone()));
                text.set(q);
            }
            true => {
                // TODO: this will reset the query, which we should confirm first
                search_params.set(SearchMode::default());
                text.set(String::new());
            }
        },
        (text.clone(), context.clone(), search_params.clone()),
    );

    UseStandardSearch {
        search_params,
        pagination,
        text,
        filter_input_state,
        onset,
        onclear,
        ontogglesimple,
    }
}

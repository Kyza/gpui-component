use std::time::Duration;
use std::{cell::Cell, rc::Rc};

use crate::Icon;
use crate::{
    input::{InputEvent, TextInput},
    scroll::{Scrollbar, ScrollbarState},
    theme::ActiveTheme,
    v_flex, IconName, Size,
};
use gpui::{
    actions, div, prelude::FluentBuilder, uniform_list, AnyElement, AppContext, Entity,
    FocusHandle, FocusableView, InteractiveElement, IntoElement, KeyBinding, Length,
    ListSizingBehavior, MouseButton, ParentElement, Render, SharedString, Styled, Task,
    UniformListScrollHandle, View, ViewContext, VisualContext, WindowContext,
};
use gpui::{px, ScrollStrategy};
use smol::Timer;

actions!(list, [Cancel, Confirm, SelectPrev, SelectNext]);

pub fn init(cx: &mut AppContext) {
    let context: Option<&str> = Some("List");
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("up", SelectPrev, context),
        KeyBinding::new("down", SelectNext, context),
    ]);
}

/// A delegate for the List.
#[allow(unused)]
pub trait ListDelegate: Sized + 'static {
    type Item: IntoElement;

    /// When Query Input change, this method will be called.
    /// You can perform search here.
    fn perform_search(&mut self, query: &str, cx: &mut ViewContext<List<Self>>) -> Task<()> {
        Task::ready(())
    }

    /// Return the number of items in the list.
    fn items_count(&self, cx: &AppContext) -> usize;

    /// Render the item at the given index.
    ///
    /// Return None will skip the item.
    fn render_item(&self, ix: usize, cx: &mut ViewContext<List<Self>>) -> Option<Self::Item>;

    /// Return a Element to show when list is empty.
    fn render_empty(&self, cx: &mut ViewContext<List<Self>>) -> impl IntoElement {
        div()
    }

    /// Returns Some(AnyElement) to render the initial state of the list.
    ///
    /// This can be used to show a view for the list before the user has interacted with it.
    ///
    /// For example: The last search results, or the last selected item.
    ///
    /// Default is None, that means no initial state.
    fn render_initial(&self, cx: &mut ViewContext<List<Self>>) -> Option<AnyElement> {
        None
    }

    /// Return the confirmed index of the selected item.
    fn confirmed_index(&self, cx: &AppContext) -> Option<usize> {
        None
    }

    /// Set the selected index, just store the ix, don't confirm.
    fn set_selected_index(&mut self, ix: Option<usize>, cx: &mut ViewContext<List<Self>>);

    /// Set the confirm and give the selected index, this is means user have clicked the item or pressed Enter.
    fn confirm(&mut self, ix: Option<usize>, cx: &mut ViewContext<List<Self>>) {}

    /// Cancel the selection, e.g.: Pressed ESC.
    fn cancel(&mut self, cx: &mut ViewContext<List<Self>>) {}
}

pub struct List<D: ListDelegate> {
    focus_handle: FocusHandle,
    delegate: D,
    max_height: Option<Length>,
    query_input: Option<View<TextInput>>,
    last_query: Option<String>,
    loading: bool,

    enable_scrollbar: bool,
    vertical_scroll_handle: UniformListScrollHandle,
    scrollbar_state: Rc<Cell<ScrollbarState>>,

    pub(crate) size: Size,
    selected_index: Option<usize>,
    right_clicked_index: Option<usize>,
    _search_task: Task<()>,
}

impl<D> List<D>
where
    D: ListDelegate,
{
    pub fn new(delegate: D, cx: &mut ViewContext<Self>) -> Self {
        let query_input = cx.new_view(|cx| {
            TextInput::new(cx)
                .appearance(false)
                .prefix(|cx| Icon::new(IconName::Search).text_color(cx.theme().muted_foreground))
                .placeholder("Search...")
                .cleanable()
        });

        cx.subscribe(&query_input, Self::on_query_input_event)
            .detach();

        Self {
            focus_handle: cx.focus_handle(),
            delegate,
            query_input: Some(query_input),
            last_query: None,
            selected_index: None,
            right_clicked_index: None,
            vertical_scroll_handle: UniformListScrollHandle::new(),
            scrollbar_state: Rc::new(Cell::new(ScrollbarState::new())),
            max_height: None,
            enable_scrollbar: true,
            loading: false,
            size: Size::default(),
            _search_task: Task::ready(()),
        }
    }

    /// Set the size
    pub fn set_size(&mut self, size: Size, cx: &mut ViewContext<Self>) {
        if let Some(input) = &self.query_input {
            input.update(cx, |input, cx| {
                input.set_size(size, cx);
            })
        }
        self.size = size;
    }

    pub fn max_h(mut self, height: impl Into<Length>) -> Self {
        self.max_height = Some(height.into());
        self
    }

    pub fn no_scrollbar(mut self) -> Self {
        self.enable_scrollbar = false;
        self
    }

    pub fn no_query(mut self) -> Self {
        self.query_input = None;
        self
    }

    pub fn set_query_input(&mut self, query_input: View<TextInput>, cx: &mut ViewContext<Self>) {
        cx.subscribe(&query_input, Self::on_query_input_event)
            .detach();
        self.query_input = Some(query_input);
    }

    pub fn delegate(&self) -> &D {
        &self.delegate
    }

    pub fn delegate_mut(&mut self) -> &mut D {
        &mut self.delegate
    }

    pub fn focus(&mut self, cx: &mut WindowContext) {
        self.focus_handle(cx).focus(cx);
    }

    pub fn set_selected_index(&mut self, ix: Option<usize>, cx: &mut ViewContext<Self>) {
        self.selected_index = ix;
        self.delegate.set_selected_index(ix, cx);
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected_index
    }

    /// Set the query_input text
    pub fn set_query(&mut self, query: &str, cx: &mut ViewContext<Self>) {
        if let Some(query_input) = &self.query_input {
            let query = query.to_owned();
            query_input.update(cx, |input, cx| input.set_text(query, cx))
        }
    }

    /// Get the query_input text
    pub fn query(&self, cx: &mut ViewContext<Self>) -> Option<SharedString> {
        self.query_input.as_ref().map(|input| input.read(cx).text())
    }

    fn render_scrollbar(&self, cx: &mut ViewContext<Self>) -> Option<impl IntoElement> {
        if !self.enable_scrollbar {
            return None;
        }

        Some(Scrollbar::uniform_scroll(
            cx.view().entity_id(),
            self.scrollbar_state.clone(),
            self.vertical_scroll_handle.clone(),
        ))
    }

    fn scroll_to_selected_item(&mut self, _cx: &mut ViewContext<Self>) {
        if let Some(ix) = self.selected_index {
            self.vertical_scroll_handle
                .scroll_to_item(ix, ScrollStrategy::Top);
        }
    }

    fn on_query_input_event(
        &mut self,
        _: View<TextInput>,
        event: &InputEvent,
        cx: &mut ViewContext<Self>,
    ) {
        match event {
            InputEvent::Change(text) => {
                let text = text.trim().to_string();
                if Some(&text) == self.last_query.as_ref() {
                    return;
                }

                self.set_loading(true, cx);
                let search = self.delegate.perform_search(&text, cx);

                self._search_task = cx.spawn(|this, mut cx| async move {
                    search.await;

                    let _ = this.update(&mut cx, |this, _| {
                        this.vertical_scroll_handle
                            .scroll_to_item(0, ScrollStrategy::Top);
                        this.last_query = Some(text);
                    });

                    // Always wait 100ms to avoid flicker
                    Timer::after(Duration::from_millis(100)).await;
                    let _ = this.update(&mut cx, |this, cx| {
                        this.set_loading(false, cx);
                    });
                });
            }
            InputEvent::PressEnter => self.on_action_confirm(&Confirm, cx),
            _ => {}
        }
    }

    fn set_loading(&mut self, loading: bool, cx: &mut ViewContext<Self>) {
        self.loading = loading;
        if let Some(input) = &self.query_input {
            input.update(cx, |input, cx| input.set_loading(loading, cx))
        }
        cx.notify();
    }

    fn on_action_cancel(&mut self, _: &Cancel, cx: &mut ViewContext<Self>) {
        self.set_selected_index(None, cx);
        self.delegate.cancel(cx);
        cx.notify();
    }

    fn on_action_confirm(&mut self, _: &Confirm, cx: &mut ViewContext<Self>) {
        if self.delegate.items_count(cx) == 0 {
            return;
        }

        self.delegate.confirm(self.selected_index, cx);
        cx.notify();
    }

    fn on_action_select_prev(&mut self, _: &SelectPrev, cx: &mut ViewContext<Self>) {
        let items_count = self.delegate.items_count(cx);
        if items_count == 0 {
            return;
        }

        let selected_index = self.selected_index.unwrap_or(0);
        if selected_index > 0 {
            self.selected_index = Some(selected_index - 1);
        } else {
            self.selected_index = Some(items_count - 1);
        }

        self.delegate.set_selected_index(self.selected_index, cx);
        self.scroll_to_selected_item(cx);
        cx.notify();
    }

    fn on_action_select_next(&mut self, _: &SelectNext, cx: &mut ViewContext<Self>) {
        let items_count = self.delegate.items_count(cx);
        if items_count == 0 {
            return;
        }

        if let Some(selected_index) = self.selected_index {
            if selected_index < items_count - 1 {
                self.selected_index = Some(selected_index + 1);
            } else {
                self.selected_index = Some(0);
            }
        } else {
            self.selected_index = Some(0);
        }

        self.delegate.set_selected_index(self.selected_index, cx);
        self.scroll_to_selected_item(cx);
        cx.notify();
    }

    fn render_list_item(&mut self, ix: usize, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let selected = self.selected_index == Some(ix);
        let right_clicked = self.right_clicked_index == Some(ix);

        div()
            .id("list-item")
            .w_full()
            .relative()
            .children(self.delegate.render_item(ix, cx))
            .when(selected || right_clicked, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .right(px(0.))
                        .bottom(px(0.))
                        .when(selected, |this| this.bg(cx.theme().list_active))
                        .border_1()
                        .border_color(cx.theme().list_active_border),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, cx| {
                    this.right_clicked_index = None;
                    this.selected_index = Some(ix);
                    this.on_action_confirm(&Confirm, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, cx| {
                    this.right_clicked_index = Some(ix);
                    cx.notify();
                }),
            )
    }
}

impl<D> FocusableView for List<D>
where
    D: ListDelegate,
{
    fn focus_handle(&self, cx: &AppContext) -> FocusHandle {
        if let Some(query_input) = &self.query_input {
            query_input.focus_handle(cx)
        } else {
            self.focus_handle.clone()
        }
    }
}

impl<D> Render for List<D>
where
    D: ListDelegate,
{
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let view = cx.view().clone();
        let vertical_scroll_handle = self.vertical_scroll_handle.clone();
        let items_count = self.delegate.items_count(cx);
        let sizing_behavior = if self.max_height.is_some() {
            ListSizingBehavior::Infer
        } else {
            ListSizingBehavior::Auto
        };

        let initial_view = if let Some(input) = &self.query_input {
            if input.read(cx).text().is_empty() {
                self.delegate().render_initial(cx)
            } else {
                None
            }
        } else {
            None
        };

        v_flex()
            .key_context("List")
            .id("list")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .overflow_hidden()
            .on_action(cx.listener(Self::on_action_cancel))
            .on_action(cx.listener(Self::on_action_confirm))
            .on_action(cx.listener(Self::on_action_select_next))
            .on_action(cx.listener(Self::on_action_select_prev))
            .when_some(self.query_input.clone(), |this, input| {
                this.child(
                    div()
                        .map(|this| match self.size {
                            Size::Small => this.py_0().px_1p5(),
                            _ => this.py_1().px_2(),
                        })
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(input),
                )
            })
            .map(|this| {
                if let Some(view) = initial_view {
                    this.child(view)
                } else {
                    this.child(
                        v_flex()
                            .flex_grow()
                            .relative()
                            .when_some(self.max_height, |this, h| this.max_h(h))
                            .overflow_hidden()
                            .when(items_count == 0, |this| {
                                this.child(self.delegate().render_empty(cx))
                            })
                            .when(items_count > 0, |this| {
                                this.child(
                                    uniform_list(view, "uniform-list", items_count, {
                                        move |list, visible_range, cx| {
                                            visible_range
                                                .map(|ix| list.render_list_item(ix, cx))
                                                .collect::<Vec<_>>()
                                        }
                                    })
                                    .flex_grow()
                                    .with_sizing_behavior(sizing_behavior)
                                    .track_scroll(vertical_scroll_handle)
                                    .into_any_element(),
                                )
                            })
                            .children(self.render_scrollbar(cx)),
                    )
                }
            })
            // Click out to cancel right clicked row
            .when(self.right_clicked_index.is_some(), |this| {
                this.on_mouse_down_out(cx.listener(|this, _, cx| {
                    this.right_clicked_index = None;
                    cx.notify();
                }))
            })
    }
}

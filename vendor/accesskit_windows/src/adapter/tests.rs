// Speakeasy patch: regressions for provider and range retirement, and range scrolling.

use super::*;
use accesskit::{Action, ActionData, ActionRequest, TextDirection, TextPosition, TextSelection};
use std::sync::mpsc::{Receiver, Sender, channel};
use windows::{Win32::System::Variant::VARIANT, core::Interface};

struct Requests(Sender<ActionRequest>);

impl ActionHandler for Requests {
    fn do_action(&mut self, request: ActionRequest) {
        self.0.send(request).unwrap();
    }
}

struct InitialTree(TreeUpdate);

impl ActivationHandler for InitialTree {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(self.0.clone())
    }
}

const TEXT: &str = "Synthetic";

fn tree(with_field: bool) -> TreeUpdate {
    let mut root = NodeProvider::new(Role::Window);
    let mut nodes = Vec::new();
    if with_field {
        root.set_children(vec![LocalNodeId(1)]);
        let mut field = NodeProvider::new(Role::TextInput);
        field.set_label("Synthetic field");
        field.set_children(vec![LocalNodeId(2)]);
        let mut run = NodeProvider::new(Role::TextRun);
        run.set_value(TEXT);
        run.set_character_lengths(vec![1; TEXT.len()]);
        run.set_text_direction(TextDirection::LeftToRight);
        nodes.push((LocalNodeId(1), field));
        nodes.push((LocalNodeId(2), run));
    }
    nodes.insert(0, (LocalNodeId(0), root));
    TreeUpdate {
        nodes,
        tree: Some(TreeData::new(LocalNodeId(0))),
        tree_id: TreeId::ROOT,
        focus: LocalNodeId(0),
    }
}

fn activate() -> (Adapter, Receiver<ActionRequest>) {
    let (requests, received) = channel();
    let mut adapter = Adapter::new(HWND::default(), false, Requests(requests));
    let activation = adapter.handle_wm_getobject(
        WPARAM(0),
        LPARAM(UiaRootObjectId as isize),
        &mut InitialTree(tree(true)),
    );
    assert!(activation.is_some());
    (adapter, received)
}

fn hide_and_show(adapter: &mut Adapter) {
    for with_field in [false, true] {
        drop(adapter.update_if_active(|| tree(with_field)));
    }
}

fn field(adapter: &Adapter) -> IRawElementProviderFragment {
    let State::Active(context) = &adapter.state else {
        panic!("the adapter did not activate");
    };
    let root_id = context.read_tree().state().root_id();
    let root: IRawElementProviderFragment = context
        .get_or_create_platform_node(root_id)
        .into_interface();
    unsafe { root.Navigate(NavigateDirection_FirstChild) }.unwrap()
}

fn name(element: &IRawElementProviderFragment) -> windows::core::Result<VARIANT> {
    let element: IRawElementProviderSimple = element.cast()?;
    unsafe { element.GetPropertyValue(UIA_NamePropertyId) }
}

fn document(element: &IRawElementProviderFragment) -> ITextRangeProvider {
    let element: IRawElementProviderSimple = element.cast().unwrap();
    let pattern: ITextProvider = unsafe { element.GetPatternProvider(UIA_TextPatternId) }
        .unwrap()
        .cast()
        .unwrap();
    unsafe { pattern.DocumentRange() }.unwrap()
}

fn text(range: &ITextRangeProvider) -> windows::core::Result<String> {
    unsafe { range.GetText(-1) }.map(|text| text.to_string())
}

fn retired<T>(result: windows::core::Result<T>) -> bool {
    result.err().map(|error| error.code()) == Some(crate::util::element_not_available().code())
}

#[test]
fn a_removed_provider_stays_retired_when_its_node_id_returns() {
    let (mut adapter, _) = activate();
    let old = field(&adapter);
    assert!(name(&old).is_ok());
    hide_and_show(&mut adapter);
    assert!(name(&field(&adapter)).is_ok());
    assert!(retired(name(&old)));
}

#[test]
fn a_retained_range_retires_with_its_provider_when_its_nodes_return() {
    let (mut adapter, requests) = activate();
    let old = document(&field(&adapter));
    let clone = unsafe { old.Clone() }.unwrap();
    assert_eq!(text(&old).unwrap(), TEXT);
    hide_and_show(&mut adapter);
    let current = document(&field(&adapter));
    assert_eq!(text(&current).unwrap(), TEXT);
    assert!(retired(text(&old)), "A retained range read the restored field");
    assert!(retired(text(&clone)), "A clone outlived its provider");
    assert!(retired(unsafe { old.Select() }));
    assert!(requests.try_recv().is_err(), "A retired range requested an action");
    assert!(retired(unsafe { old.GetEnclosingElement() }));
    assert!(retired(unsafe { current.Compare(&old) }));
    assert!(unsafe { current.Compare(&current) }.unwrap().as_bool());
    let endpoint = TextPatternRangeEndpoint_Start;
    assert!(retired(unsafe {
        current.CompareEndpoints(endpoint, &old, endpoint)
    }));
    assert!(retired(unsafe {
        current.MoveEndpointByRange(endpoint, &old, endpoint)
    }));
}

#[test]
fn scrolling_a_range_into_view_names_it_with_the_aligned_end_as_focus() {
    let (adapter, requests) = activate();
    let range = document(&field(&adapter));
    unsafe {
        range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, 2)
    }
    .unwrap();
    let at = |character_index| TextPosition {
        node: LocalNodeId(2),
        character_index,
    };
    for (align_to_top, anchor, focus) in [(true, at(9), at(2)), (false, at(2), at(9))] {
        unsafe { range.ScrollIntoView(align_to_top.into()) }.unwrap();
        let request = requests.try_recv().unwrap();
        assert_eq!(request.action, Action::ScrollIntoView);
        assert_eq!(request.target_node, LocalNodeId(2));
        assert_eq!(
            request.data,
            Some(ActionData::SetTextSelection(TextSelection { anchor, focus }))
        );
    }
}

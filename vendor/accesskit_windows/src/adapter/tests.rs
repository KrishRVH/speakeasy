// Speakeasy patch: regressions for provider retirement.

use super::*;
use accesskit::ActionRequest;
use windows::{Win32::System::Variant::VARIANT, core::Interface};

struct NoActions;

impl ActionHandler for NoActions {
    fn do_action(&mut self, _request: ActionRequest) {}
}

struct InitialTree(TreeUpdate);

impl ActivationHandler for InitialTree {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(self.0.clone())
    }
}

fn tree(with_field: bool) -> TreeUpdate {
    let mut root = NodeProvider::new(Role::Window);
    let mut nodes = Vec::new();
    if with_field {
        root.set_children(vec![LocalNodeId(1)]);
        let mut field = NodeProvider::new(Role::TextInput);
        field.set_label("Synthetic field");
        nodes.push((LocalNodeId(1), field));
    }
    nodes.insert(0, (LocalNodeId(0), root));
    TreeUpdate {
        nodes,
        tree: Some(TreeData::new(LocalNodeId(0))),
        tree_id: TreeId::ROOT,
        focus: LocalNodeId(0),
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

#[test]
fn a_removed_provider_stays_retired_when_its_node_id_returns() {
    let mut adapter = Adapter::new(HWND::default(), false, NoActions);
    let activation = adapter.handle_wm_getobject(
        WPARAM(0),
        LPARAM(UiaRootObjectId as isize),
        &mut InitialTree(tree(true)),
    );
    assert!(activation.is_some());
    let retired = field(&adapter);
    assert!(name(&retired).is_ok());
    for with_field in [false, true] {
        drop(adapter.update_if_active(|| tree(with_field)));
    }
    assert!(name(&field(&adapter)).is_ok());
    assert_eq!(
        name(&retired).err().map(|error| error.code()),
        Some(crate::util::element_not_available().code())
    );
}

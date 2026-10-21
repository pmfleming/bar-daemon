//! Genuine StatusNotifierItem; ksni owns the D-Bus menu and watcher recovery.
pub(super) struct Tray;
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "shelllist-updates".into()
    }
    fn title(&self) -> String {
        "Shelllist Updates".into()
    }
    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }
    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }
    fn icon_name(&self) -> String {
        "shelllist-updates".into()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Shelllist Updates".into(),
            description: "Review recent system and AI-tool updates".into(),
            ..Default::default()
        }
    }
    fn activate(&mut self, _: i32, _: i32) {
        launch();
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            ksni::menu::StandardItem {
                label: "Open update history".into(),
                activate: Box::new(|_| launch()),
                ..Default::default()
            }
            .into(),
        ]
    }
}
fn launch() {
    let token = ksni::current_xdg_activation_token();
    tokio::spawn(async move {
        if let Err(error) = super::open("", token.as_deref()).await {
            tracing::warn!(%error, "Could not open Updates");
        }
    });
}

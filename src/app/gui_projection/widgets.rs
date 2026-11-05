use super::App;
use crate::ipc::app_surface::protocol::WidgetRef;

impl App {
    pub(crate) fn desktop_widget(
        &mut self,
        workspace: &str,
        item: WidgetRef,
    ) -> Result<u64, String> {
        let stale = || "widget changed; reopen the action".to_string();
        if !self
            .workspaces
            .get(self.active_ws)
            .is_some_and(|w| w.id == workspace && w.remote.is_none())
            || self.preparing_registered_workspace()
        {
            return Err(stale());
        }
        match item {
            WidgetRef::Dock {
                key,
                owner,
                index,
                action,
                text,
                value,
                menu,
            } => {
                if self.module_owning_dock(&key).as_deref() != Some(&owner) {
                    return Err(stale());
                }
                let row = self
                    .module_docks
                    .get(&key)
                    .and_then(|d| d.rows.get(usize::from(index)))
                    .ok_or_else(stale)?;
                let (selected, payload) = if let Some(i) = menu {
                    let it = row
                        .menu
                        .get(usize::from(i))
                        .filter(|it| !it.is_divider())
                        .ok_or_else(stale)?;
                    (
                        Some(it.action.as_str()),
                        it.value.as_ref().or(row.value.as_ref()),
                    )
                } else {
                    (row.action.as_deref(), row.value.as_ref())
                };
                if row.text != text
                    || selected != Some(action.as_str())
                    || payload != value.as_ref()
                {
                    return Err(stale());
                }
                let extra = vec![
                    ("LUVUS_MODULE_DOCK_ID".into(), key),
                    ("LUVUS_MODULE_ROW_INDEX".into(), index.to_string()),
                    ("LUVUS_MODULE_ROW_TEXT".into(), text.clone()),
                    ("LUVUS_MODULE_ROW_VALUE".into(), value.unwrap_or(text)),
                    ("LUVUS_MODULE_ACTION_ID".into(), action.clone()),
                ];
                self.module_invoke_dock_action(&action, Some(&owner), extra)
            }
            WidgetRef::Bar {
                key,
                owner,
                index,
                action,
                value,
                compact,
            } => {
                let widget = self
                    .bar
                    .widgets
                    .get(&key)
                    .filter(|w| w.key.owner == owner)
                    .ok_or_else(stale)?;
                let segments = if compact {
                    &widget.compact_content
                } else {
                    &widget.content
                };
                let segment = segments.get(usize::from(index)).ok_or_else(stale)?;
                if segment.action.as_deref() != Some(&action)
                    || segment.click_value() != value.as_deref()
                {
                    return Err(stale());
                }
                let mut extra = vec![
                    ("LUVUS_MODULE_BAR_ID".into(), widget.key.id.clone()),
                    ("LUVUS_MODULE_BAR_SEGMENT".into(), index.to_string()),
                ];
                if let Some(value) = value {
                    extra.push(("LUVUS_MODULE_BAR_VALUE".into(), value));
                }
                self.module_invoke_action_with(&action, Some(&owner), "bar", extra)
            }
        }
    }
}

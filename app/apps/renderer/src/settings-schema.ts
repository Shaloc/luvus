export interface SettingSpec {
  path: string; category: string; group: string; title: string;
  kind: 'toggle'|'choice'|'number'|'text'|'editor'|'provider';
  options?: Record<string,string>; min?: number; max?: number; unit?: string; scale?: number;
}

// Explicit metadata keeps internal/migration fields out of the user-facing form.
export const settingSpecs: SettingSpec[] = [
  {"path":"check_updates","category":"general","group":"Updates","title":"Check for updates","kind":"toggle"},
  {"path":"notifications.display","category":"general","group":"Notifications","title":"Notification display","kind":"choice","options":{"inbox":"Inbox only","toast":"Toast"}},
  {"path":"notifications.sound_style","category":"general","group":"Notifications","title":"Notification sound","kind":"choice","options":{"retro":"Retro","soft":"Soft","pulse":"Pulse"}},
  {"path":"notifications.sound_on_done","category":"general","group":"Notifications","title":"Sound on completion","kind":"toggle"},
  {"path":"notifications.sound_on_blocked","category":"general","group":"Notifications","title":"Sound when blocked","kind":"toggle"},
  {"path":"notifications.terminal_on_agent","category":"general","group":"Notifications","title":"Terminal notifications","kind":"toggle"},
  {"path":"session.persist_pane_screen","category":"general","group":"Session recovery","title":"Restore terminal contents","kind":"toggle"},
  {"path":"theme_appearance","category":"appearance","group":"Core theme","title":"Core display mode","kind":"choice","options":{"auto":"Follow system","light":"Light","dark":"Dark"}},
  {"path":"theme_terminal_background","category":"appearance","group":"Core theme","title":"Apply terminal background","kind":"toggle"},
  {"path":"shell","category":"terminal","group":"Shell and input","title":"Shell command","kind":"text"},
  {"path":"allow_nested","category":"terminal","group":"Shell and input","title":"Allow nested Luvus","kind":"toggle"},
  {"path":"layout.shift_enter","category":"terminal","group":"Shell and input","title":"Modified Enter encoding","kind":"choice","options":{"esc-cr":"ESC CR (default)","lf":"LF (newline)","esc-lf":"ESC LF","csi-u":"CSI-u"}},
  {"path":"layout.new_pane_copy_on_select","category":"terminal","group":"Selection","title":"Copy selection in new panes","kind":"toggle"},
  {"path":"layout.scrollback_bytes","category":"terminal","group":"History","title":"Scrollback memory per pane","kind":"number","min":1,"max":256,"unit":"MiB","scale":1048576},
  {"path":"layout.new_pane_to_workspace_root","category":"workspace","group":"Opening and recovery","title":"Start new panes at workspace root","kind":"toggle"},
  {"path":"layout.resume_in_new_workspace","category":"workspace","group":"Opening and recovery","title":"Resume in a new workspace","kind":"toggle"},
  {"path":"layout.auto_workspace_rehome","category":"workspace","group":"Workspace organization","title":"Automatically organize workspaces","kind":"toggle"},
  {"path":"layout.workspace_display","category":"workspace","group":"Workspace organization","title":"Workspace grouping","kind":"choice","options":{"tree":"By host","flat":"Flat list"}},
  {"path":"layout.workspace_paths","category":"workspace","group":"Workspace organization","title":"Show workspace paths","kind":"toggle"},
  {"path":"worktree.provider","category":"workspace","group":"Worktrees","title":"Worktree provider","kind":"provider"},
  {"path":"layout.file_click","category":"files","group":"File browser","title":"Single-click files","kind":"choice","options":{"preview":"Preview","tab":"Tab"}},
  {"path":"layout.file_open","category":"files","group":"File browser","title":"Open files with","kind":"editor"},
  {"path":"layout.files_show_hidden","category":"files","group":"File browser","title":"Show hidden files","kind":"toggle"},
  {"path":"layout.diff_layout","category":"files","group":"Diff review","title":"Diff arrangement","kind":"choice","options":{"auto":"Automatic","split":"Side by side","stack":"Stacked"}},
  {"path":"layout.diff_color_mode","category":"files","group":"Diff review","title":"Diff colors","kind":"choice","options":{"theme":"Follow theme","standard":"Standard red and green"}},
  {"path":"layout.diff_marker_style","category":"files","group":"Diff review","title":"Change markers","kind":"choice","options":{"symbols":"Symbols","bars":"Color bars","both":"Symbols and bars"}},
  {"path":"layout.diff_context_lines","category":"files","group":"Diff review","title":"Context lines","kind":"number","min":0,"max":20,"unit":"lines"},
  {"path":"layout.diff_wrap","category":"files","group":"Diff review","title":"Wrap long lines","kind":"toggle"},
  {"path":"layout.diff_show_line_numbers","category":"files","group":"Diff review","title":"Show line numbers","kind":"toggle"},
  {"path":"layout.diff_live_refresh","category":"files","group":"Diff review","title":"Refresh changes automatically","kind":"toggle"},
  {"path":"agents_active_only","category":"agents","group":"Agent list","title":"Only live agents","kind":"toggle"},
  {"path":"agents_this_workspace","category":"agents","group":"Agent list","title":"Only this workspace","kind":"toggle"},
  {"path":"agents_status_filter.working","category":"agents","group":"Visible states","title":"Working","kind":"toggle"},
  {"path":"agents_status_filter.blocked","category":"agents","group":"Visible states","title":"Blocked","kind":"toggle"},
  {"path":"agents_status_filter.idle","category":"agents","group":"Visible states","title":"Idle","kind":"toggle"},
  {"path":"agents_status_filter.done","category":"agents","group":"Visible states","title":"Done","kind":"toggle"},
  {"path":"layout.agent_title","category":"agents","group":"Agent labels","title":"Show session title","kind":"toggle"},
  {"path":"layout.agent_title_first","category":"agents","group":"Agent labels","title":"Prefer session title","kind":"toggle"},
  {"path":"layout.agent_paths","category":"agents","group":"Agent labels","title":"Show working directory","kind":"toggle"},
  {"path":"layout.agent_status_names","category":"agents","group":"Agent labels","title":"Show status text","kind":"toggle"},
  {"path":"commander_working_policy","category":"agents","group":"Agent behavior","title":"When the agent is busy","kind":"choice","options":{"ask":"Ask","auto_send":"Send automatically"}},
  {"path":"resume_launch_flags","category":"agents","group":"Agent behavior","title":"Reuse launch options when resuming","kind":"toggle"},
  {"path":"remote_auto_install","category":"connections","group":"Remote hosts","title":"Automatically install remote Luvus","kind":"toggle"},
  {"path":"layout.show_titles","category":"advanced","group":"Pane frames","title":"Show pane titles","kind":"toggle"},
  {"path":"layout.pane_title_path","category":"advanced","group":"Pane frames","title":"Show paths in pane titles","kind":"toggle"},
  {"path":"layout.rounded_corners","category":"advanced","group":"Pane frames","title":"Rounded corners","kind":"toggle"},
  {"path":"layout.col_gap","category":"advanced","group":"TUI layout","title":"Column gap","kind":"number","min":0,"max":1,"unit":"cells"},
  {"path":"layout.row_gap","category":"advanced","group":"TUI layout","title":"Row gap","kind":"number","min":0,"max":1,"unit":"cells"},
  {"path":"layout.mobile_width","category":"advanced","group":"TUI layout","title":"Compact layout threshold","kind":"number","min":0,"max":200,"unit":"columns"},
  {"path":"sidebars.left.visible","category":"advanced","group":"TUI sidebars","title":"Show left sidebar","kind":"toggle"},
  {"path":"sidebars.right.visible","category":"advanced","group":"TUI sidebars","title":"Show right sidebar","kind":"toggle"},
  {"path":"sidebars.left.width","category":"advanced","group":"TUI sidebars","title":"Left sidebar width","kind":"number","min":18,"max":44,"unit":"columns"},
  {"path":"sidebars.right.width","category":"advanced","group":"TUI sidebars","title":"Right sidebar width","kind":"number","min":18,"max":44,"unit":"columns"},
];
export function settingValue(config: Record<string,unknown>, path: string): unknown {
  return path.split('.').reduce<unknown>((value,key)=> value && typeof value === 'object' ? (value as Record<string,unknown>)[key] : undefined, config);
}
export function numericSetting(draft: string, spec: SettingSpec): number {
  const value = Number(draft);
  if (!draft.trim() || !Number.isFinite(value) || !Number.isInteger(value) || value < spec.min! || value > spec.max!) throw Error('Invalid numeric setting');
  return value * (spec.scale ?? 1);
}

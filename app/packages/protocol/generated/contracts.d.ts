export type LuvusAppProtocol = Request | Response;
export type Request =
  | {
      id: string;
      type: "snapshot";
    }
  | {
      generation: string;
      id: string;
      method:
        | "uhp.capabilities"
        | "uhp.stats"
        | "uhp.token.create"
        | "uhp.token.list"
        | "uhp.token.revoke"
        | "ping"
        | "server.stop"
        | "server.reload_config"
        | "server.agent_manifests"
        | "server.reload_agent_manifests"
        | "config.get"
        | "config.patch"
        | "session.snapshot"
        | "events.subscribe"
        | "events.wait"
        | "wait.output"
        | "workspace.list"
        | "workspace.get"
        | "workspace.new"
        | "workspace.open"
        | "workspace.focus"
        | "workspace.rename"
        | "workspace.pin"
        | "workspace.move"
        | "workspace.move_block"
        | "workspace.report_metadata"
        | "workspace.close"
        | "node.list"
        | "node.new"
        | "node.open"
        | "node.focus"
        | "node.rename"
        | "node.pin"
        | "node.close"
        | "tab.list"
        | "tab.get"
        | "tab.new"
        | "tab.focus"
        | "tab.move"
        | "tab.swap"
        | "tab.rename"
        | "tab.close"
        | "pane.list"
        | "pane.get"
        | "pane.current"
        | "pane.layout"
        | "pane.neighbor"
        | "pane.edges"
        | "pane.split"
        | "pane.move"
        | "pane.swap"
        | "pane.transpose"
        | "pane.focus"
        | "pane.focus_direction"
        | "pane.resize"
        | "pane.zoom"
        | "pane.rename"
        | "pane.run"
        | "pane.send_input"
        | "pane.read"
        | "pane.status"
        | "pane.processes"
        | "pane.report_session"
        | "pane.release_session"
        | "pane.report_event"
        | "pane.restart"
        | "pane.close"
        | "attach.pane"
        | "layout.export"
        | "layout.apply"
        | "layout.set_split_ratio"
        | "agent.list"
        | "agent.get"
        | "agent.explain"
        | "agent.report"
        | "agent.release"
        | "agent.start"
        | "agent.prompt"
        | "agent.wait"
        | "agent.name"
        | "agent.fork"
        | "agent.send"
        | "agent.keys"
        | "agent.read"
        | "agent.sessions"
        | "agent.resume"
        | "search"
        | "search.capabilities"
        | "search.query"
        | "search.activate"
        | "files.tree"
        | "files.open"
        | "files.reveal"
        | "files.refresh"
        | "git.status"
        | "git.branches"
        | "git.log"
        | "git.open"
        | "mission.snapshot"
        | "mission.refresh"
        | "mission.open"
        | "diff.refresh"
        | "diff.list"
        | "diff.open"
        | "diff.get"
        | "diff.navigate"
        | "diff.note.add"
        | "diff.note.apply"
        | "diff.note.list"
        | "diff.note.edit"
        | "diff.note.resolve"
        | "diff.note.reopen"
        | "diff.note.remove"
        | "diff.note.send"
        | "worktree.list"
        | "worktree.create"
        | "worktree.open"
        | "worktree.remove"
        | "task.add"
        | "task.list"
        | "task.get"
        | "task.claim"
        | "task.next"
        | "task.start"
        | "task.heartbeat"
        | "task.update"
        | "task.done"
        | "task.retry"
        | "task.merge"
        | "task.release"
        | "task.delete"
        | "automation.create"
        | "automation.list"
        | "automation.get"
        | "automation.update"
        | "automation.enable"
        | "automation.disable"
        | "automation.rebind"
        | "automation.delete"
        | "automation.run"
        | "automation.history"
        | "automation.preview"
        | "automation.health"
        | "lease.acquire"
        | "lease.list"
        | "lease.release"
        | "module.list"
        | "module.info"
        | "module.link"
        | "module.update"
        | "module.unlink"
        | "module.uninstall"
        | "module.enable"
        | "module.disable"
        | "module.action.list"
        | "module.action.invoke"
        | "module.pane.open"
        | "module.pane.focus"
        | "module.pane.close"
        | "module.config_dir"
        | "module.settings.list"
        | "module.settings.get"
        | "module.settings.set"
        | "module.log.list"
        | "theme.list"
        | "theme.path"
        | "theme.use"
        | "theme.reload"
        | "manifest.reload"
        | "ui.sidebar"
        | "ui.agent_title.push"
        | "ui.agent_title.clear"
        | "ui.dock.push"
        | "ui.dock.list"
        | "ui.dock.move"
        | "ui.bar.push"
        | "ui.bar.list"
        | "ui.bar.move"
        | "ui.bar.remove"
        | "ui.notification.push"
        | "ui.notification.clear"
        | "ui.toast"
        | "browser.open"
        | "browser.panel"
        | "browser.list"
        | "browser.get"
        | "browser.navigate"
        | "browser.back"
        | "browser.forward"
        | "browser.reload"
        | "browser.close"
        | "browser.snapshot"
        | "browser.act"
        | "browser.evaluate"
        | "browser.screenshot"
        | "browser.wait"
        | "browser.dialog"
        | "browser.control.acquire"
        | "browser.control.release"
        | "browser.control.status"
        | "browser.cdp.subscribe"
        | "browser.cdp.connect"
        | "browser.cdp.send"
        | "terminal.backend.inventory"
        | "terminal.backend.snapshot"
        | "terminal.backend.validate"
        | "terminal.backend.processes"
        | "terminal.backend.capture"
        | "terminal.backend.observe"
        | "terminal.backend.control"
        | "terminal.backend.type_literal"
        | "terminal.backend.paste_text"
        | "terminal.backend.submit_text"
        | "terminal.backend.send_key"
        | "terminal.backend.set_title"
        | "terminal.backend.notify"
        | "terminal.backend.create"
        | "terminal.backend.close"
        | "terminal.backend.wait_change"
        | "terminal.backend.wait_output"
        | "terminal.backend.events.subscribe";
      params: {};
      type: "action";
    }
  | {
      id: string;
      input: Input;
      target: Target;
      type: "input";
    }
  | {
      cell_height: number;
      cell_width: number;
      cols: number;
      id: string;
      rows: number;
      target: Target;
      type: "viewport";
    }
  | {
      activate?: boolean;
      id: string;
      target: Target;
      type: "focus";
    }
  | {
      forward: boolean;
      id: string;
      target: Target;
      type: "focus_history";
    }
  | {
      id: string;
      sequence: string;
      target: Target;
      type: "graphic";
    }
  | {
      generation: string;
      id: string;
      number?: number;
      type: "pull_requests";
      workspace_id: string;
    }
  | {
      client: boolean;
      generation: string;
      id: string;
      type: "logs";
    }
  | {
      generation: string;
      id: string;
      item:
        | {
            action: string;
            index: number;
            key: string;
            kind: "dock";
            menu?: number | null;
            owner: string;
            text: string;
            value: string | null;
          }
        | {
            action: string;
            compact: boolean;
            index: number;
            key: string;
            kind: "bar";
            owner: string;
            value: string | null;
          };
      type: "widget";
      workspace_id: string;
    }
  | {
      generation: string;
      id: string;
      operation:
        | {
            kind: "expand" | "collapse" | "markdown" | "mermaid" | "inspect";
          }
        | {
            kind: "create_file" | "create_directory";
            name: string;
          }
        | {
            identity: string;
            kind: "rename";
            name: string;
          }
        | {
            identity: string;
            kind: "delete";
          };
      path: string;
      root: string;
      type: "file_entry";
      workspace_id: string;
    }
  | {
      id: string;
      target: Target;
      type: "browser_open";
    }
  | {
      browser_id: string;
      id: string;
      type: "browser_closed";
    }
  | {
      browser_id: string;
      id: string;
      lease_generation?: string;
      type: "browser_takeover";
    }
  | {
      error: string | null;
      id: string;
      lease_generation: string;
      target: Target;
      type: "browser_result";
      value: unknown;
    }
  | {
      browser_id: string;
      chunk: CdpChunk;
      id: string;
      lease_generation: string;
      target: Target;
      type: "browser_cdp_event";
    }
  | {
      id: string;
      type: "detach";
    }
  | {
      id: string;
      receipt: string;
      type: "clipboard_succeeded";
    };
export type Input =
  | {
      kind: "paste";
      text: string;
    }
  | {
      alt?: boolean;
      ctrl?: boolean;
      key: string;
      kind: "key";
      release?: boolean;
      repeat?: boolean;
      shift?: boolean;
    }
  | {
      kind: "scroll";
      lines: number;
    }
  | {
      action: "press" | "release" | "move" | "wheel_up" | "wheel_down";
      alt?: boolean;
      button: number;
      col: number;
      ctrl?: boolean;
      geometry: string;
      kind: "mouse";
      row: number;
      shift?: boolean;
    }
  | {
      end: CellPoint;
      geometry: string;
      kind: "selection";
      mode: "cell" | "word" | "line";
      revision: string;
      start: CellPoint;
    }
  | {
      geometry: string;
      kind: "copy";
    }
  | {
      focused: boolean;
      kind: "focus_event";
    };
/**
 * @minItems 2
 * @maxItems 2
 */
export type CellPoint = [number, number];
export type Response =
  | {
      capabilities: string[];
      protocol: number;
      transport: number;
      type: "ready";
    }
  | {
      snapshot: {};
      type: "snapshot";
    }
  | {
      frame: TerminalFrame;
      type: "terminal";
    }
  | {
      id: string;
      result: unknown;
      type: "reply";
    }
  | {
      code: string;
      id: string | null;
      message: string;
      type: "error";
    }
  | {
      data: unknown;
      kind: string;
      type: "effect";
    }
  | {
      index: number;
      text: string;
      total: number;
      transfer: string;
      type: "chunk";
    };

export interface Target {
  generation: string;
  pane_id: string;
  tab_id: string;
  terminal_id: string | null;
  workspace_id: string;
}
export interface CdpChunk {
  index: number;
  text: string;
  total: number;
  transfer: string;
}
export interface TerminalFrame {
  cells: Cell[];
  cols: number;
  cursor: Cursor | null;
  geometry: string;
  /**
   * @maxItems 128
   */
  graphics?: {
    /**
     * @maxItems 150000
     */
    cells: [number, number, number, number][];
    cols: number;
    rows: number;
    sequence: string;
  }[];
  history_rows: number;
  modes: TerminalModes;
  revision: string;
  rows: number;
  scroll_offset: number;
  target: Target;
  /**
   * @maxItems 300
   */
  wraps: boolean[];
}
export interface Cell {
  bg: number;
  col: number;
  fg: number;
  link: string | null;
  mods: number;
  row: number;
  text: string;
  width: number;
}
export interface Cursor {
  col: number;
  row: number;
  shape: "block" | "beam" | "underline";
  visible: boolean;
}
export interface TerminalModes {
  alternate_screen: boolean;
  alternate_scroll: boolean;
  focus_reporting: boolean;
  mouse: boolean;
  mouse_drag: boolean;
  mouse_motion: boolean;
}

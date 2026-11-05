import { tr } from './i18n';
import { useState } from 'react';
import { Bot, Check, Play, Plus, RefreshCw, RotateCcw } from 'lucide-react';
import { action, focusTarget, mutate, useQuery } from './bridge';
import { topologyDialog, validateContext, type OpenDialog } from './Actions';
import { array, numeric, record, text, truth, type Pane, type RecordValue, type Snapshot, type Workspace } from './model';
import { AsyncButton, Detail, Empty, ErrorBox, IconButton, Loading, Notice, StatusDot } from './ui';

interface Props { snapshot: Snapshot; workspace?: Workspace; openDialog: OpenDialog }
export function MissionPanel({ snapshot }: Props) {
  const query = useQuery('mission.snapshot', { scope: 'all' });
  const data = record(query.data), summary = record(data.summary), coverage = record(summary.usage_coverage);
  return <section className="domain-panel"><div className="panel-toolbar"><h2>{tr("Mission Control")}</h2><AsyncButton run={async () => { await action('mission.refresh', { scope: 'all' }); query.reload(); }}><RefreshCw size={14} />{tr("Refresh usage")}</AsyncButton></div>
    {query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : <>
      <div className="metric-grid"><div><small>{tr("Agents")}</small><strong>{numeric(summary.agents)}</strong></div><div><small>{tr("Recorded tokens")}</small><strong>{numeric(coverage.available) ? numeric(summary.tokens).toLocaleString() : '—'}</strong></div><div><small>{tr("Recorded cost")}</small><strong>{numeric(coverage.priced) ? `$${numeric(summary.cost_usd).toFixed(2)}` : '—'}</strong></div></div>
      <p className="muted small">{tr("Usage available for")} {numeric(coverage.available)} {tr("of")} {numeric(coverage.total)} {tr("agents. Costs are from persisted counters, not estimates.")}</p>
      {truth(data.refreshing) && <Notice>{tr("Usage refresh is running in the core. Refresh this view for the latest counters.")}</Notice>}
      {array(data.rows).map((value, i) => { const row = record(value), usage = record(row.usage); const pane = snapshot.workspaces.find((w) => w.id === row.workspace_id)?.tabs.flatMap((t) => t.panes).find((p) => p.id === row.pane); return <div className="fleet-row" key={i}><StatusDot state={text(row.state)} /><div><strong>{text(row.agent)}</strong><small>{text(row.workspace_name)} · {text(row.kind)}</small></div><div><strong>{typeof usage.total_tokens === 'number' ? usage.total_tokens.toLocaleString() : '—'}</strong><small>{text(row.usage_status)}</small></div>{pane?.target && <AsyncButton run={() => focusTarget(pane.target!)}>{tr("Focus")}</AsyncButton>}</div>; })}
      {!array(data.rows).length && <Empty title={tr("No agents in this session")}>{tr("Live agents and resumable sessions will appear here.")}</Empty>}
    </>}
  </section>;
}
export function AgentPanel({ snapshot, workspace, openDialog, pane }: Props & { pane: Pane | null }) {
  const [mode, setMode] = useState<'details' | 'evidence' | 'processes' | 'history'>('details');
  const method = mode === 'history' ? 'agent.sessions' : mode === 'processes' ? 'pane.processes' : mode === 'evidence' ? 'agent.explain' : 'agent.get';
  const query = useQuery(method, mode === 'history' ? {} : mode === 'processes' ? { pane: pane?.id } : { target: pane?.id }, Boolean(pane?.target) || mode === 'history');
  const tab = workspace?.tabs.find((t) => t.panes.some((p) => p.id === pane?.id));
  return <section className="domain-panel"><div className="panel-toolbar"><h2>{pane?.title ?? tr("Agents")}</h2><IconButton label={tr("Refresh agent data")} onClick={query.reload}><RefreshCw size={14} /></IconButton></div>
    <div className="segmented" role="tablist" aria-label={tr("Agent views")}>{(['details', 'evidence', 'processes', 'history'] as const).map((item) => <button key={item} role="tab" aria-selected={mode === item} onClick={() => setMode(item)}>{item}</button>)}</div>
    {mode !== 'history' && !pane ? <Empty title={tr("Select an agent")}>{tr("Use an agent’s details button in the sidebar, or browse resumable history.")}</Empty> : query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : mode === 'history' ? <>
      {array(record(query.data).sessions).map((value, i) => { const session = record(value); return <div className="list-card" key={i}><Bot size={16} /><div><strong>{text(session.agent)}</strong><p>{text(session.cwd)}</p><code>{text(session.session_id)}</code></div><button className="button" onClick={() => openDialog({ title: `Resume ${text(session.agent)}?`, description: text(session.cwd), submit: tr("Resume"), run: () => mutate('agent.resume', { session_id: session.session_id }, snapshot.generation), done: query.reload })}>{tr("Resume")}</button></div>; })}
      {!array(record(query.data).sessions).length && <Empty title={tr("No resumable native sessions found")} />}
    </> : <Detail value={query.data} />}
    {pane?.isAgent && workspace && tab && <div className="button-row"><button className="button primary" onClick={() => openDialog(topologyDialog('agent-prompt', snapshot, workspace, tab, pane))}>{tr("Send prompt…")}</button><button className="button" onClick={() => openDialog({ title: tr("Fork agent session?"), description: tr("The core will use the agent’s native fork support and create a sibling terminal. Unsupported agents return an explicit error."), submit: tr("Fork session"), run: () => { validateContext(snapshot, workspace, tab, pane); return mutate('agent.fork', { target: pane.id, focus: true }, snapshot.generation); } })}>{tr("Fork…")}</button></div>}
  </section>;
}
export function OrchPanel({ snapshot, workspace, openDialog }: Props) {
  const [mode, setMode] = useState<'tasks' | 'automations' | 'leases' | 'worktrees'>('tasks');
  const query = useQuery(mode === 'tasks' ? 'task.list' : mode === 'automations' ? 'automation.list' : mode === 'leases' ? 'lease.list' : 'worktree.list');
  const data = record(query.data);
  const agents=workspace?.tabs.flatMap(t=>t.panes.filter(p=>p.isAgent && p.target?.terminal_id).map(p=>({t,p})))??[];
  const rebind=(item:RecordValue)=>openDialog({title:tr("Rebind agent terminal"),submit:tr("Rebind"),description:tr("The owner must verify the same native session before changing this binding."),fields:[{key:'pane',label:tr("Agent terminal"),kind:'select',required:true,options:agents.map(({p})=>({value:p.id,label:p.title+' · '+p.id}))}],run:v=>{
    const selected=agents.find(({p})=>p.id===v.pane);if(!workspace||!selected)throw Error('Select a current agent terminal');
    validateContext(snapshot,workspace,selected.t,selected.p,true);
    return mutate('automation.rebind',{id:item.id,pane:selected.p.id,terminal_id:selected.p.target!.terminal_id},snapshot.generation);
  },done:query.reload});
  const confirm = (method: 'task.done' | 'task.retry' | 'task.delete' | 'task.merge', id: string, title: string) => openDialog({ title, submit: tr("Confirm"), destructive: method === 'task.delete' || method === 'task.merge', description: `Task ${id}. The core validates task state, ownership and quality gates.`, run: () => mutate(method, { id }, snapshot.generation), done: query.reload });
  return <section className="domain-panel"><div className="panel-toolbar"><h2>{tr("Orchestration")}</h2><IconButton label={tr("Refresh orchestration")} onClick={query.reload}><RefreshCw size={14} /></IconButton></div>
    <div className="segmented" role="tablist" aria-label={tr("Orchestration views")}>{(['tasks', 'automations', 'leases', 'worktrees'] as const).map((item) => <button role="tab" aria-selected={mode === item} key={item} onClick={() => setMode(item)}>{item}</button>)}</div>
    {mode === 'tasks' && <button className="button" disabled={!workspace} onClick={() => openDialog({ title: tr("Create task"), submit: tr("Create task"), fields: [{ key: 'title', label: tr("Task title"), required: true }, { key: 'prompt', label: tr("Briefing"), kind: 'textarea' }, { key: 'paths', label: tr("Paths"), hint: tr("One relative path per line."), kind: 'textarea' },{key:'gate',label:tr("Quality gate command")}], run: (v) => mutate('task.add', { title: v.title, prompt: v.prompt, paths: text(v.paths).split('\n').filter(Boolean), ...(text(v.gate)?{gate:v.gate}:{}), workspace_id: workspace?.id }, snapshot.generation), done: query.reload })}><Plus size={14} />{tr("New task")}</button>}
    {query.loading ? <Loading /> : query.error ? <ErrorBox error={query.error} /> : mode === 'tasks' ? <div className="task-board">
      {array(data.tasks).map((value) => { const task = record(value), id = text(task.id); return <article className="task-card" key={id}><div className="task-meta"><code>{id}</code><span className="badge">{text(task.status)}</span></div><h3>{text(task.title)}</h3>{text(task.prompt) && <p>{text(task.prompt)}</p>}<small className="muted">{tr("Attempt")} {numeric(task.attempt)}{text(task.assignee) && ` · ${text(task.assignee)}`}</small><div className="button-row">
        <button className="button" disabled={!workspace} onClick={() => openDialog({ title: tr("Start task worker"), submit: tr("Start worker"), description: tr("Workspace mode shares this checkout. Worktree mode creates an isolated worktree under the explicit parent workspace."), fields: [{ key: 'agent', label: tr("Agent kind"), required: true }, { key: 'mode', label: tr("Execution mode"), kind: 'select', value: 'worktree', options: [{ value: 'worktree', label: tr("Isolated worktree") }, { value: 'workspace', label: tr("Shared workspace") }] }], run: (v) => mutate('task.start', { id, agent: v.agent, mode: v.mode, workspace_id: workspace?.id, focus: true }, snapshot.generation), done: query.reload })}><Play size={13} />{tr("Start")}</button>
        <button className="button" onClick={() => confirm('task.done', id, 'Complete task and run its quality gate?')}><Check size={13} />{tr("Done")}</button><button className="button" onClick={() => confirm('task.retry', id, 'Retry task?')}><RotateCcw size={13} />{tr("Retry")}</button>
        <details className="dropdown"><summary className="button">{tr("More")}</summary><div className="dropdown-menu"><button onClick={() => confirm('task.merge', id, 'Merge task worktree?')}>{tr("Merge…")}</button><button className="destructive-text" onClick={() => confirm('task.delete', id, 'Delete task?')}>{tr("Delete…")}</button></div></details>
      </div><details className="disclosure"><summary>{tr("Task details")}</summary><Detail value={task} /></details></article>; })}
      {!array(data.tasks).length && <Empty title={tr("No tasks yet")}>{tr("Create a task with a briefing and explicit workspace.")}</Empty>}
    </div> : mode === 'automations' ? <>
      {workspace && !workspace.host && <button className="button" disabled={!agents.length} onClick={()=>{
        openDialog({title:tr("Schedule an agent prompt"),submit:tr("Schedule"),fields:[
          {key:'name',label:tr("Name"),required:true},{key:'prompt',label:tr("Prompt"),kind:'textarea',required:true},
          {key:'pane',label:tr("Agent terminal"),kind:'select',required:true,options:agents.map(({p})=>({value:p.id,label:p.title+' · '+p.id}))},
          {key:'at',label:tr("Run at"),required:true,value:new Date(Date.now()+3600_000).toISOString()},
          {key:'busy',label:tr("When busy"),kind:'select',value:'wait',options:[{value:'wait',label:tr("Wait for readiness")},{value:'skip',label:tr("Skip this run")}]}],
          run:v=>{
            const agent=agents.find(({p})=>p.id===v.pane);if(!agent)throw Error('Agent terminal changed');
            validateContext(snapshot,workspace,agent.t,agent.p,true);
            const at=Math.floor(Date.parse(text(v.at))/1000);if(!Number.isSafeInteger(at)||at<=Date.now()/1000)throw Error('Choose a future time');
            return mutate('automation.create',{name:v.name,trigger:{kind:'once',at_utc:at},
              target:{kind:'active_agent',pane_id:agent.p.id,terminal_id:agent.p.target!.terminal_id,if_busy:v.busy},
              task:{title:v.name,prompt:v.prompt,agent_id:agent.p.agent,workspace_id:workspace.id,mode:'workspace',access:'workspace'}},snapshot.generation);
          },done:query.reload});
      }}>{tr("Schedule agent prompt…")}</button>}
      {array(data.automations).map((value, i) => { const automation = record(value); return <article className="task-card" key={i}><h3>{text(automation.name) || text(automation.id)}</h3><Detail value={automation} /><div className="button-row"><AsyncButton run={async () => { await mutate(automation.enabled === true ? 'automation.disable' : 'automation.enable', { id: automation.id }, snapshot.generation); query.reload(); }}>{automation.enabled === true ? tr("Disable") : tr("Enable")}</AsyncButton><button className="button" onClick={() => openDialog({ title: tr("Run automation now?"), description: tr("This may start a worker or deliver a prompt according to the saved policy."), submit: tr("Run"), run: () => mutate('automation.run', { id: automation.id }, snapshot.generation), done: query.reload })}>{tr("Run now…")}</button>{record(automation.target).binding==='durable' && <button className="button" disabled={!agents.length || record(automation.task).workspace_id!==workspace?.id} onClick={()=>rebind(automation)}>{tr("Rebind terminal…")}</button>}</div></article>; })}
      {!array(data.automations).length && <Empty title={tr("No scheduled automations")} />}
      <Notice>{tr("Use the structured automation tools to create or change schedules and policies.")}</Notice>
    </> : <Detail value={query.data} />}
  </section>;
}

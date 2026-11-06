import { tr } from './i18n';
import { useState } from 'react';
import { ExternalLink, Search } from 'lucide-react';
import { safeLink } from '@luvus/terminal-view';
import { mutate } from './bridge';
import { array, numeric, page, record, targetKey, text, truth, type Pane, type RecordValue } from './model';
import { AsyncButton, Empty, ErrorBox, Loading, Notice } from './ui';
import { BrowserView } from './BrowserView';

export function NativeView({ pane }: { pane: Pane }) {
  const [find, setFind] = useState('');
  const data = pane.data, state = text(data.state);
  const openLink = (url: string) => {
    const safe = safeLink(url);
    if (!safe || !pane.target) return Promise.reject(new Error(tr("Only HTTP(S) links from an available pane can be opened.")));
    return mutate('browser.open', { pane: pane.id, url: safe }, pane.target.generation);
  };
  if (state === 'loading') return <Loading />;
  if (state === 'error') return <ErrorBox error={text(data.error, 'The core could not load this view.')} />;
  if (state === 'unavailable') return <Empty title={tr("This view is not available in the desktop projection")}>{text(data.reason).replaceAll('_', ' ')}{tr(". The original leaf is preserved; no terminal fallback is used.")}</Empty>;
  if (['binary', 'too_large', 'conflict'].includes(state)) return <Empty title={state === 'binary' ? tr("Binary file") : state === 'conflict' ? tr("Conflicted file") : tr("File exceeds the preview limit")}>{text(data.message) || `${numeric(data.bytes).toLocaleString()} bytes`}</Empty>;
  if (pane.kind === 'browser') return <BrowserView key={pane.target ? targetKey(pane.target) : pane.id} pane={pane} />;
  if (pane.kind === 'diff') return <div className="native-content"><DiffContent data={data} paginated />{truth(data.text_truncated) && <Notice>{tr("Core preview budget reached; content is truncated.")}</Notice>}</div>;
  if (pane.kind === 'preview') return <article className="document-view">{page(data.blocks).map((block, i) => <Block key={i} block={block} openLink={openLink} />)}{truth(record(data.blocks).truncated) && <Notice>{tr("The core sent a bounded document preview. Further pages are not available in this protocol.")}</Notice>}</article>;
  if (pane.kind !== 'file') return <Empty title={tr("No native content supplied")}>{tr("This leaf needs a semantic projection from the core.")}</Empty>;
  const rows = page(data.rows), offset = numeric(record(data.rows).offset);
  return <div className="native-content">
    <div className="native-toolbar"><Search size={14} /><input aria-label={tr("Find in projected file page")} placeholder={tr("Find in this page…")} value={find} onChange={(event) => setFind(event.target.value)} /><span>{numeric(record(data.rows).total).toLocaleString()} {tr("lines")}</span></div>
    <div className={`file-lines ${truth(data.wrap) ? 'wrap-lines' : ''}`} tabIndex={0} aria-label={text(data.path, 'File content')}>{rows.map((row, index) => {
      const content = text(row.text), matches = find && content.toLocaleLowerCase().includes(find.toLocaleLowerCase());
      return <div key={offset + index} className={matches ? 'search-match' : ''}><span className="line-number" aria-hidden="true">{offset + index + 1}</span><code>{content || ' '}</code></div>;
    })}</div>
    {(truth(record(data.rows).truncated) || truth(data.text_truncated)) && <Notice>{tr("Showing the core’s bounded page. Native page navigation is not yet exposed by this protocol.")}</Notice>}
  </div>;
}

export function DiffContent({ data, paginated = false }: { data: RecordValue; paginated?: boolean }) {
  const hunks = paginated ? page(data.hunks) : array(data.hunks).map(record);
  if (truth(data.binary)) return <Empty title={tr("Binary change")}>{tr("A text diff is not available.")}</Empty>;
  if(data.split_rows) return <div className="diff-content" aria-label={tr("Split diff")}>{page(data.split_rows).map((row,i)=><div className="split-diff-row" key={i}>
    {(['old','new'] as const).map(side=>{const line=record(row[side]);return <div key={side} className={'diff-line '+text(line.kind)} data-side={side}><span className="line-number">{String(line[side+'_line']??'')}</span><code>{text(line.text)}</code></div>;})}
  </div>)}{(truth(record(data.split_rows).truncated)||truth(data.source_truncated)) && <Notice>{tr("Showing a bounded page of the split diff.")}</Notice>}</div>;
  return <div className="diff-content">
    <div className="diff-summary"><span className="added">+{numeric(data.additions)}</span><span className="deleted">−{numeric(data.deletions)}</span></div>
    {hunks.map((hunk, index) => <section key={text(hunk.id) || index}>
      <div className="hunk-header">{text(hunk.header)}</div>
      {(paginated ? page(hunk.lines) : array(hunk.lines).map(record)).map((line, i) => <div key={i} className={`diff-line ${text(line.kind) === 'addition' ? 'addition' : text(line.kind) === 'deletion' ? 'deletion' : ''}`}><span className="line-number">{typeof line.old_line === 'number' ? line.old_line : ''}</span><span className="line-number">{typeof line.new_line === 'number' ? line.new_line : ''}</span><code>{text(line.kind) === 'addition' ? '+' : text(line.kind) === 'deletion' ? '−' : ' '} {text(line.text)}</code></div>)}
      {paginated && truth(record(hunk.lines).truncated) && <Notice>{tr("Hunk truncated by the core.")}</Notice>}
    </section>)}
    {(truth(data.truncated) || truth(data.source_truncated) || (paginated && truth(record(data.hunks).truncated))) && <Notice>{tr("This diff is truncated.")} {numeric(data.omitted_lines)} {tr("lines omitted.")}</Notice>}
    {!hunks.length && <Empty title={tr("No text changes")} />}
  </div>;
}
function Inline({ value, openLink }: { value: unknown; openLink: (url: string) => Promise<unknown> }) {
  return <>{page(value).map((span, index) => {
    const role = text(span.role), content = text(span.text), link = safeLink(text(span.link_target));
    if (link) return <AsyncButton key={index} className="inline-link" run={() => openLink(link)}>{content}<ExternalLink size={11} /></AsyncButton>;
    if (role.includes('code')) return <code key={index}>{content}</code>;
    if (role.includes('strong')) return <strong key={index}>{content}</strong>;
    if (role.includes('emphasis')) return <em key={index}>{content}</em>;
    return <span key={index}>{content}</span>;
  })}</>;
}
function Block({ block, openLink }: { block: RecordValue; openLink: (url: string) => Promise<unknown> }) {
  const content = <Inline value={block.content} openLink={openLink} />;
  switch (text(block.kind)) {
    case 'heading': return <div role="heading" aria-level={Math.max(1, Math.min(6, numeric(block.level, 2)))} className={`document-heading heading-${numeric(block.level, 2)}`}>{content}</div>;
    case 'paragraph': return <p>{content}</p>;
    case 'quote': return <blockquote>{content}</blockquote>;
    case 'list_item': return <div className="document-list-item" style={{ paddingLeft: `${Math.min(10, numeric(block.depth)) * 16}px` }}><span>{typeof block.checked === 'boolean' ? (block.checked ? '[x]' : '[ ]') : text(block.marker, '•')}</span><div>{content}</div></div>;
    case 'rule': return <hr />;
    case 'code': return <pre><small>{text(block.language)}</small><code>{text(block.text)}</code></pre>;
    case 'source_fallback': return <><Notice>{text(block.diagnostic)}</Notice><pre>{text(block.text)}</pre></>;
    case 'image_placeholder': return <Notice>{tr("Image not loaded:")} {text(block.alt) || text(block.target)}</Notice>;
    case 'table': return <div className="table-scroll"><table><thead><tr>{array(record(block.header).items).map((cell, i) => <th key={i}><Inline value={cell} openLink={openLink} /></th>)}</tr></thead><tbody>{array(record(block.rows).items).map((row, i) => <tr key={i}>{array(record(row).items).map((cell, j) => <td key={j}><Inline value={cell} openLink={openLink} /></td>)}</tr>)}</tbody></table></div>;
    case 'mermaid': return text(block.diagram_kind) === 'flowchart' ? <Flowchart block={block} /> : <><Notice>{tr("Sequence diagram source — semantic events are not projected yet.")}</Notice><pre>{text(block.text)}</pre></>;
    default: return <Notice>{tr("Unsupported semantic block:")} {text(block.kind)}</Notice>;
  }
}
function Flowchart({ block }: { block: RecordValue }) {
  const nodes = page(block.nodes), edges = page(block.edges);
  const width = 640, columns = Math.min(3, Math.max(1, nodes.length)), height = Math.ceil(nodes.length / columns) * 100 + 32;
  const point = (i: number) => ({ x: 24 + (i % columns) * (width / columns), y: 24 + Math.floor(i / columns) * 100 });
  return <svg className="flowchart" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={tr("Flowchart from core semantic data")}>
    {edges.map((edge, i) => {
      const a = typeof edge.from === 'number' ? edge.from : nodes.findIndex((node) => node.id === edge.from);
      const b = typeof edge.to === 'number' ? edge.to : nodes.findIndex((node) => node.id === edge.to);
      if (a < 0 || b < 0 || !nodes[a] || !nodes[b]) return null;
      const from = point(a), to = point(b);
      return <g key={i}><line x1={from.x + 70} y1={from.y + 32} x2={to.x + 70} y2={to.y + 32} stroke="currentColor" strokeDasharray={truth(edge.dotted) ? '4 4' : undefined} /><text x={(from.x + to.x) / 2 + 70} y={(from.y + to.y) / 2 + 16}>{text(edge.label)}</text></g>;
    })}
    {nodes.map((node, i) => { const p = point(i); return <g key={i}><rect x={p.x} y={p.y} width={140} height={64} rx={text(node.shape).includes('round') ? 24 : 8} /><text x={p.x + 70} y={p.y + 36}>{text(node.label).slice(0, 24)}</text></g>; })}
  </svg>;
}

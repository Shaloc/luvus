import { useEffect, useRef, useState } from 'react';
import { Search } from 'lucide-react';
import { action, mutate } from './bridge';
import { array, message, record, text, truth, type Snapshot } from './model';
import { AsyncButton, Empty, ErrorBox, Loading, Notice } from './ui';

export function SearchPanel({ snapshot }: { snapshot: Snapshot }) {
  const [query, setQuery] = useState(''), [scope, setScope] = useState('all');
  const [result, setResult] = useState<unknown>(null), [error, setError] = useState<string | null>(null), [loading, setLoading] = useState(false);
  const revision = useRef(0);
  const workspace = snapshot.workspaces.find(w=>w.active)?.id;
  useEffect(() => { revision.current += 1; setResult(null);setLoading(false);setError(null); return () => { revision.current += 1; }; }, [snapshot.generation, workspace]);
  return <section className="domain-panel"><h2>Global finder</h2><form className="search-form" onSubmit={(event) => {
    event.preventDefault();
    const generation = ++revision.current;
    if (!query.trim() || new TextEncoder().encode(query).length > 256) { setError('Enter a query of 1–256 UTF-8 bytes.'); return; }
    setLoading(true); setError(null);
    void action('search.query', { query, scope, limit: 100 }, snapshot.generation).then((data) => { if (generation === revision.current) setResult(data); }).catch((error: unknown) => { if (generation === revision.current) setError(message(error)); }).finally(() => { if (generation === revision.current) setLoading(false); });
  }}><label className="field"><span>Search query</span><input autoFocus value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Files, panes, agents, retained output…" required /></label><div className="button-row"><select aria-label="Search scope" value={scope} onChange={(event) => setScope(event.target.value)}><option value="all">Everything</option><option value="navigate">Navigation</option><option value="files">Files</option><option value="output">Terminal output</option></select><button className="button primary" disabled={loading}><Search size={14} />Search</button></div></form>
    {loading && <Loading />}{error && <ErrorBox error={error} />}
    {array(record(result).matches).map((value, i) => { const item = record(value); return <AsyncButton className="search-result" key={text(item.id) || i} disabled={Boolean(item.owner_session || item.host)} run={() => mutate('search.activate', { kind: item.kind, target: item.target }, snapshot.generation)}><span className="badge">{text(item.kind)}</span><span><strong>{text(item.label)}</strong><small>{text(item.detail)}</small></span></AsyncButton>; })}
    {truth(record(result).partial) && <Notice>The core returned a bounded, partial result set.</Notice>}
    {result !== null && !loading && !array(record(result).matches).length && <Empty title="No matches" />}
  </section>;
}

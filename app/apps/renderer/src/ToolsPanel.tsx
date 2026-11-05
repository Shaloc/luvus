import { useState } from 'react';
import { Search } from 'lucide-react';
import requestSchema from '../../../../protocol/uhp/v1/schema/request.schema.json';
import { action, useQuery, type Method } from './bridge';
import { array, label, record, text, type RecordValue } from './model';
import { AsyncButton, Detail, ErrorBox, Loading, Notice } from './ui';

const documentSchema = record(requestSchema);
function resolveSchema(value: unknown, depth = 0): RecordValue {
  if (depth > 20) return {};
  const schema = record(value), ref = text(schema.$ref);
  if (ref.startsWith('#/$defs/')) return resolveSchema(record(documentSchema.$defs)[ref.slice(8)], depth + 1);
  const combined: RecordValue = { ...schema };
  for (const part of array(schema.allOf)) {
    const resolved = resolveSchema(part, depth + 1);
    Object.assign(combined, resolved, { properties: { ...record(combined.properties), ...record(resolved.properties) }, required: [...array(combined.required), ...array(resolved.required)] });
  }
  return combined;
}
function methodSchema(method: string): RecordValue | null {
  for (const condition of array(documentSchema.allOf)) {
    const entry = record(condition), match = record(record(record(entry.if).properties).method);
    if (match.const === method || array(match.enum).includes(method)) return resolveSchema(record(record(entry.then).properties).params);
  }
  return null;
}
const knownMethods = new Set(array(record(record(documentSchema.properties).method).enum).filter((v): v is string => typeof v === 'string'));
/** Low-frequency automation only. Main domains have purpose-built views. */
export function ToolsPanel() {
  const capabilities = useQuery('uhp.capabilities');
  const [filter, setFilter] = useState(''), [method, setMethod] = useState('automation.create');
  const [params, setParams] = useState<unknown>({}), [result, setResult] = useState<unknown>(null), [confirmed, setConfirmed] = useState(false);
  const methods = array(record(capabilities.data).methods).filter((v): v is string => typeof v === 'string' && knownMethods.has(v) && !v.startsWith('node.') && !v.startsWith('browser.'));
  const schema = methodSchema(method);
  const contracts = record(record(capabilities.data).method_contracts);
  const contract = record(contracts[method]);
  return <section className="domain-panel"><h2>Structured tools</h2><p className="muted">Low-frequency UHP operations. Fields come from the checked-in core request schema; results come from the connected core.</p>
    {capabilities.loading ? <Loading /> : capabilities.error ? <ErrorBox error={capabilities.error} /> : <>
      <div className="filter-input"><Search size={14} /><input aria-label="Filter UHP methods" placeholder="Filter methods…" value={filter} onChange={(event) => setFilter(event.target.value)} /></div>
      <label className="field"><span>Method</span><select value={method} onChange={(event) => { setMethod(event.target.value); setParams({}); setResult(null); setConfirmed(false); }}>{methods.filter((item) => item.includes(filter) || item === method).map((item) => <option key={item}>{item}</option>)}</select></label>
      {schema ? <><SchemaFields key={method} schema={schema} value={params} onChange={setParams} name="params" />
        <label className="check-label"><input type="checkbox" checked={confirmed} onChange={(event) => setConfirmed(event.target.checked)} />I reviewed this operation and its target. It may change core state.</label>
        <AsyncButton className="button primary" disabled={!confirmed || !methods.includes(method)} run={async () => { validateFields(schema, params, 'params'); const value = await action(method as Method, record(params)); setResult(value); setConfirmed(false); }}>Execute {method}</AsyncButton>
      </> : <Notice>The current public schema has no field contract for this method. It cannot be invoked through the generic form; use its dedicated interface where available.</Notice>}
      {Object.keys(contract).length > 0 && <details className="disclosure"><summary>Method contract</summary><Detail value={contract} /></details>}
      {result !== null && <div className="tool-result"><h3>Core result</h3><Detail value={result} /></div>}
    </>}
  </section>;
}
function SchemaFields({ schema: raw, value, onChange, name, depth = 0 }: { schema: RecordValue; value: unknown; onChange: (value: unknown) => void; name: string; depth?: number }) {
  const schema = resolveSchema(raw), [variant, setVariant] = useState(0);
  const variants = array(schema.oneOf ?? schema.anyOf).map(record);
  if (depth > 10) return <Notice>Schema exceeds the nested form limit.</Notice>;
  if (variants.length) return <div className="schema-group"><label className="field"><span>{label(name)} format</span><select value={variant} onChange={(event) => { setVariant(Number(event.target.value)); onChange(undefined); }}>{variants.map((item, i) => { const s = resolveSchema(item), properties = record(s.properties); return <option key={i} value={i}>{text(s.title) || text(record(properties.kind).const) || text(record(properties.type).const) || text(s.type, `Option ${i + 1}`)}</option>; })}</select></label><SchemaFields schema={variants[variant] ?? {}} value={value} onChange={onChange} name={name} depth={depth + 1} /></div>;
  if (schema.const !== undefined) return <div className="field"><span>{label(name)}: <code>{String(schema.const)}</code></span><AsyncButton run={async () => onChange(schema.const)}>Use {String(schema.const)}</AsyncButton></div>;
  if (schema.type === 'object' || schema.properties) {
    const current = record(value), required = array(schema.required);
    return <div className="schema-group">{Object.entries(record(schema.properties)).map(([key, field]) => <div key={key} className="schema-property"><div className="schema-property-heading"><strong>{label(key)}</strong>{required.includes(key) ? <small>Required</small> : <label className="check-label"><input type="checkbox" aria-label={`Include ${key}`} checked={current[key] !== undefined} onChange={(event) => { const next = { ...current }; if (event.target.checked) { const spec = resolveSchema(field); next[key] = spec.default ?? spec.const ?? (spec.type === 'object' ? {} : spec.type === 'boolean' ? false : ''); } else delete next[key]; onChange(next); }} />Include</label>}</div>
      {(required.includes(key) || current[key] !== undefined) && <SchemaFields schema={record(field)} value={current[key]} onChange={(item) => onChange({ ...current, [key]: item })} name={key} depth={depth + 1} />}
    </div>)}{schema.maxProperties === 0 && <p className="muted">No parameters.</p>}</div>;
  }
  const options = array(schema.enum);
  if (options.length) return <label className="field"><select aria-label={name} value={value === undefined ? '' : String(value)} onChange={(event) => { const option = options.find((item) => String(item) === event.target.value); onChange(option); }}><option value="">Choose…</option>{options.map((item, i) => <option key={i} value={String(item)}>{String(item)}</option>)}</select></label>;
  if (schema.type === 'boolean') return <label className="check-label"><input type="checkbox" checked={value === true} onChange={(event) => onChange(event.target.checked)} />{label(name)}</label>;
  if (schema.type === 'array' || !schema.type || Array.isArray(schema.type)) return <JsonField value={value} onChange={onChange} name={name} />;
  return <label className="field"><input aria-label={name} type={schema.type === 'integer' || schema.type === 'number' ? 'number' : 'text'} step={schema.type === 'integer' ? 1 : undefined} min={typeof schema.minimum === 'number' ? schema.minimum : undefined} max={typeof schema.maximum === 'number' ? schema.maximum : undefined} pattern={text(schema.pattern) || undefined} maxLength={typeof schema.maxLength === 'number' ? schema.maxLength : undefined} value={value === undefined ? '' : String(value)} onChange={(event) => onChange(schema.type === 'integer' || schema.type === 'number' ? Number(event.target.value) : event.target.value)} />{(schema.description || schema.$comment) ? <small>{text(schema.description) || text(schema.$comment)}</small> : null}</label>;
}
function validateFields(raw: RecordValue, value: unknown, path: string, depth = 0): void {
  if (depth > 16) throw new Error(`${path} is nested too deeply.`);
  const schema = resolveSchema(raw), variants = array(schema.oneOf ?? schema.anyOf);
  if (variants.length) {
    for (const option of variants) {
      try { validateFields(record(option), value, path, depth + 1); return; } catch { /* Try another declared variant. */ }
    }
    throw new Error(`${path} does not match an allowed schema variant.`);
  }
  if (schema.const !== undefined && value !== schema.const) throw new Error(`${path} must be ${String(schema.const)}.`);
  if (array(schema.enum).length && !array(schema.enum).includes(value)) throw new Error(`Choose a valid ${path}.`);
  const types = typeof schema.type === 'string' ? [schema.type] : array(schema.type);
  if (types.length && !types.some((type) => type === 'null' ? value === null : type === 'object' ? value !== null && typeof value === 'object' && !Array.isArray(value) : type === 'array' ? Array.isArray(value) : type === 'integer' ? typeof value === 'number' && Number.isSafeInteger(value) : type === 'number' ? typeof value === 'number' && Number.isFinite(value) : typeof value === type)) throw new Error(`${path} must be ${types.join(' or ')}.`);
  if (schema.type === 'object' || schema.properties) {
    const item = record(value);
    for (const key of array(schema.required)) if (typeof key === 'string' && item[key] === undefined) throw new Error(`${path}.${key} is required.`);
    for (const [key, child] of Object.entries(record(schema.properties))) if (item[key] !== undefined) validateFields(record(child), item[key], `${path}.${key}`, depth + 1);
  }
  if (Array.isArray(value) && schema.items) value.forEach((item, index) => validateFields(record(schema.items), item, `${path}[${index}]`, depth + 1));
  if (typeof value === 'string') {
    if (typeof schema.minLength === 'number' && Array.from(value).length < schema.minLength) throw new Error(`${path} is too short.`);
    if (typeof schema.maxLength === 'number' && Array.from(value).length > schema.maxLength) throw new Error(`${path} is too long.`);
    if (typeof schema.pattern === 'string' && !new RegExp(schema.pattern).test(value)) throw new Error(`${path} does not match its required format.`);
  }
  if (typeof value === 'number' && ((typeof schema.minimum === 'number' && value < schema.minimum) || (typeof schema.maximum === 'number' && value > schema.maximum))) throw new Error(`${path} is outside the allowed range.`);
}
function JsonField({ value, onChange, name }: { value: unknown; onChange: (value: unknown) => void; name: string }) {
  const [draft, setDraft] = useState(value === undefined ? '' : JSON.stringify(value, null, 2)), [error, setError] = useState<string | null>(null);
  return <label className="field"><span>{label(name)} (structured JSON)</span><textarea rows={4} value={draft} onChange={(event) => { setDraft(event.target.value); try { onChange(JSON.parse(event.target.value)); setError(null); } catch { onChange(undefined); setError('Enter valid JSON before executing.'); } }} />{error && <ErrorBox error={error} />}</label>;
}

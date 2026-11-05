import chinese from './zh-CN.json';

// Only pass application-owned copy here. Session names, file paths, PTY
// cells and structured API results retain their owner's exact spelling.
export function tr(source: string) {
  return document.documentElement.lang === 'zh-CN' && Object.hasOwn(chinese, source)
    ? chinese[source as keyof typeof chinese] : source;
}

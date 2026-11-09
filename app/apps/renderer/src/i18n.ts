import chinese from './zh-CN.json';
import settings from './settings-copy.json';

// Only pass application-owned copy here. Session names, file paths, PTY
// cells and structured API results retain their owner's exact spelling.
export function tr(source: string) {
  if (document.documentElement.lang === 'zh-CN' && Object.hasOwn(settings, source)) return settings[source as keyof typeof settings];
  return document.documentElement.lang === 'zh-CN' && Object.hasOwn(chinese, source)
    ? chinese[source as keyof typeof chinese] : source;
}

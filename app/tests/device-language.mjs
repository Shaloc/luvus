// Pin the isolated device profile so English locators also work on a Chinese Mac.
export async function useEnglishUi(page) {
  await page.locator('.desktop-shell').waitFor({state:'attached'});
  const changed = await page.evaluate(() => {
    const key = 'luvus.desktop.appearance.v1';
    const prefs = JSON.parse(localStorage.getItem(key) || '{}');
    if (prefs.language === 'en') return false;
    localStorage.setItem(key, JSON.stringify({...prefs, language:'en'}));
    return true;
  });
  if (changed) await page.reload();
}

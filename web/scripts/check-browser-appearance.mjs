import assert from "node:assert/strict";

/** Runs against a live, isolated native terminal in the existing browser harness. */
export async function checkBrowserAppearance({ cdp, sessionId, run, until, click, key, screenshot }) {
  const system = async (value) => {
    await cdp.call("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value }] }, sessionId);
    await until(() => run(`matchMedia('(prefers-color-scheme: ${value})').matches`));
  };
  const theme = async (expected) => {
    await until(() => run(`document.documentElement.dataset.theme === '${expected}'`));
    assert.equal(await run("getComputedStyle(document.documentElement).colorScheme"), expected);
    assert.equal(await run("getComputedStyle(document.querySelector('.terminal-output')).backgroundColor"), expected === "light" ? "rgb(255, 255, 255)" : "rgb(13, 14, 20)");
  };
  await system("dark");
  await theme("dark");
  assert.equal(await run("document.documentElement.dataset.appearance"), "system");
  await run(`(() => {
    window.appearanceOutput = document.querySelector('.terminal-output');
    window.appearanceOutput.focus();
    const range = document.createRange();
    range.selectNodeContents(document.querySelector('.terminal-content'));
    getSelection().removeAllRanges();
    getSelection().addRange(range);
    window.appearanceSelection = getSelection().toString();
  })()`);
  await system("light");
  await theme("light");
  assert.equal(await run("window.appearanceOutput === document.querySelector('.terminal-output') && document.activeElement === window.appearanceOutput && !!window.appearanceSelection && getSelection().toString() === window.appearanceSelection"), true, "system theme changes retain terminal DOM, focus and selection");
  assert.match(await run("document.querySelector('.appearance-toggle').title"), /System \(light\)/);
  await run("getSelection().removeAllRanges(); document.querySelector('.terminal-input-proxy').focus()");
  await cdp.call("Input.insertText", { text: "printf '\\033[31mWEB_THEME_RED\\033[0m \\033[38;2;12;34;56mWEB_THEME_RGB\\033[0m \\033[7mWEB_THEME_REVERSE\\033[0m\\n'" }, sessionId);
  await key("Enter", "Enter", 13);
  const style = (marker, property = "color") => run(`(() => {
    const span = Array.from(document.querySelectorAll('.terminal-content span')).find(node => node.textContent.trim() === '${marker}' && node.style.color);
    return span && getComputedStyle(span).${property};
  })()`);
  try {
    await until(async () => await style("WEB_THEME_RED") === "rgb(179, 50, 73)");
  } catch (cause) {
    await screenshot("terminal-theme-failed");
    const output = await run("document.querySelector('.terminal-content').innerHTML.slice(-5000)");
    throw new Error(`Live ANSI theme did not arrive: ${output}`, { cause });
  }
  assert.equal(await style("WEB_THEME_RGB"), "rgb(12, 34, 56)");
  assert.equal(await style("WEB_THEME_REVERSE"), "rgb(255, 255, 255)");
  assert.equal(await style("WEB_THEME_REVERSE", "backgroundColor"), "rgb(48, 54, 66)");
  await cdp.call("Input.insertText", { text: "printf '\\033[31m中🧭é\\033[0m|WEB_CELLS\\n'" }, sessionId);
  await key("Enter", "Enter", 13);
  let cells;
  try {
    // A shell can echo the CJK character while escaping the emoji/combining
    // mark. Wait for the complete printf output, not an earlier command echo.
    await until(async () => {
      cells = await run(`(() => {
        const start = Array.from(document.querySelectorAll('.terminal-content span')).find(node =>
          node.textContent === '中' && node.nextSibling?.textContent === '🧭'
          && node.nextSibling?.nextSibling?.textContent === 'é'
          && node.nextSibling?.nextSibling?.nextSibling?.textContent.startsWith('|WEB_CELLS'));
        if (!start) return null;
        const nodes = [start, start.nextSibling, start.nextSibling.nextSibling];
        const cell = document.querySelector('.terminal-cell-probe').getBoundingClientRect().width / 10;
        const range = document.createRange();
        range.setStartBefore(nodes[0]); range.setEndAfter(nodes[2]);
        return {cell, copied:range.toString(), glyphs:nodes.map(node => ({text:node.textContent, width:node.getBoundingClientRect().width, color:getComputedStyle(node).color}))};
      })()`);
      return cells !== null;
    });
  } catch (cause) {
    await screenshot("terminal-cells-failed");
    const output = await run("document.querySelector('.terminal-content').innerHTML.slice(-6000)");
    throw new Error(`Live terminal glyph output did not arrive: ${output}`, { cause });
  }
  assert.equal(cells.copied, "中🧭é", "cell positioning preserves literal copied text");
  for (const [index, expected] of [2, 2, 1].entries()) {
    assert.ok(Math.abs(cells.glyphs[index].width - expected * cells.cell) < .15, JSON.stringify(cells));
    assert.equal(cells.glyphs[index].color, "rgb(179, 50, 73)");
  }
  await screenshot("terminal-light");

  await click(".appearance-toggle"); // System -> Light
  await until(() => run("document.documentElement.dataset.appearance === 'light'"));
  await system("dark");
  await theme("light");
  assert.equal(await run("localStorage.getItem('luvus.web.appearance')"), "light");
  await click(".appearance-toggle"); // Light -> Dark
  await theme("dark");
  assert.equal(await style("WEB_THEME_RED"), "rgb(237, 143, 158)");
  assert.equal(await style("WEB_THEME_RGB"), "rgb(12, 34, 56)", "truecolor stays application-owned");
  assert.equal(await style("WEB_THEME_REVERSE"), "rgb(13, 14, 20)");
  assert.equal(await style("WEB_THEME_REVERSE", "backgroundColor"), "rgb(217, 215, 231)");
  await screenshot("terminal-dark");
  await click(".appearance-toggle"); // Dark -> System
  await system("light");
  await theme("light");
  await click(".appearance-toggle"); // Save explicit Light for reload/cross-tab checks.
  await theme("light");
  console.log("browser appearance passed: system changes, manual override, saved choice, live ANSI/RGB/inverse colors, and preserved terminal selection/focus");
}

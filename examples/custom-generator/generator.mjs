// A ~30-line custom site generator — a SiteGeneratorAdapter driven by the fig
// bun runner (`fig render --generator ts:generator.mjs`). It reaches the engine
// through the SAME contract the browser editor uses: `content` (Liquid/kramdown
// via the wasm Session) and `fragments` (publisher-grade snapshot/diff/dict
// tables via first-include-miss). Your chrome, the engine's fragments.
//
// The runner builds ctx = { engine, fragments, content, project } exactly as the
// editor's App.tsx does; you implement init/listPages/renderPage/assetBytes.

export default {
  id: 'my-generator',
  label: 'Minimal custom generator',
  ctx: null,

  async init(ctx) {
    this.ctx = ctx;
  },

  async listPages() {
    return [{ file: 'index.html' }, { file: 'guidance.html' }];
  },

  async renderPage(file) {
    const c = this.ctx.content;
    if (file === 'index.html') {
      // ContentApi: kramdown markdown, Jekyll semantics, in the engine.
      const body = await c.renderMarkdown('# My IG\n\nWelcome to the *custom* site.');
      return { html: chrome('Home', body) };
    }
    // ContentApi: Liquid with caller globals, engine-first includes.
    const body = await c.renderLiquid('<p>Rendered at {{ now }} by {{ tool }}.</p>', {
      now: '2026', tool: 'fig',
    });
    return { html: chrome('Guidance', body) };
  },

  async assetBytes() { return null; },
};

// Your own presentation layer — the engine owns semantics, you own chrome.
function chrome(title, body) {
  return `<!doctype html><html><head><title>${title}</title></head>\n<body>\n${body}\n</body></html>`;
}

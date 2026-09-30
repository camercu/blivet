// What counts as agent attribution. Shared by commitlint.config.js, which
// rejects it at commit time, and check-agent-attribution.mjs, which rejects it
// anywhere in history, so the two match the same trailers.

// The exact trailers Claude Code writes, each anchored to the start of a
// line: prose that mentions them is not attribution. Nor is a co-author named
// Claude outside anthropic.com. One at anthropic.com is taken for the agent,
// since its display name varies by model ("Claude Opus 4.1").
const PATTERN = new RegExp(
  [
    String.raw`^Claude-Session: https?://claude\.(?:ai|com)/`,
    String.raw`^Co-Authored-By:\s+Claude\b[^<\n]*<[^>\n]*@anthropic\.com>`,
    String.raw`^\S*\s*Generated with \[Claude Code\]`,
  ].join('|'),
  'im',
);

module.exports = { PATTERN };

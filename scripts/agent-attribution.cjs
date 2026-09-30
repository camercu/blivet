// What counts as agent attribution. Shared by commitlint.config.js, which
// rejects it at commit time, and check-agent-attribution.mjs, which rejects it
// anywhere in history, so the two match the same trailers.

// The exact trailers Claude Code writes, each anchored to the start of a
// line: prose that mentions them, or a human co-author named Claude, is not
// attribution.
const PATTERN = new RegExp(
  [
    String.raw`^Claude-Session: https://claude\.ai/`,
    String.raw`^Co-Authored-By: Claude\b[^<\n]*<noreply@anthropic\.com>`,
    String.raw`^\S*\s*Generated with \[Claude Code\]`,
  ].join('|'),
  'im',
);

// Dependabot bodies quote upstream release notes and long links, which break
// commitlint's length rules; commitlint skips them. The history scan does not.
const isDependabot = (message) => message.includes('Signed-off-by: dependabot[bot]');

module.exports = { PATTERN, isDependabot };

// What counts as agent attribution, and which commits are exempt. Shared by
// commitlint.config.js, which rejects it at commit time, and
// check-agent-attribution.mjs, which rejects it anywhere in history, so the
// two cannot disagree.
const PATTERN = /^(Claude-Session:|Co-Authored-By:.*(Claude|anthropic\.com)|.*Generated with \[?Claude)/im;

// Dependabot writes its own bodies, quoting upstream release notes that may
// contain anything; it cannot add these trailers itself.
const isDependabot = (message) => message.includes('Signed-off-by: dependabot[bot]');

module.exports = { PATTERN, isDependabot };

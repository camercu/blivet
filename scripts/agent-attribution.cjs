// The trailers an agent session appends to a commit: a Claude-Session link,
// a Claude co-author, a "Generated with Claude" line. Shared by
// commitlint.config.js, which rejects them at commit time, and
// check-agent-attribution.mjs, which rejects them anywhere in history.
module.exports = /^(Claude-Session:|Co-Authored-By:.*(Claude|anthropic\.com)|.*Generated with \[?Claude)/im;

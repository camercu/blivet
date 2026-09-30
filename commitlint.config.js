// Agent attribution trailers leak session links and tool credits into
// history and release notes: 50 commits once carried a Claude-Session link
// that ended up in the v0.14.0 notes. .claude/settings.json turns them off at
// the source; this rejects any that get through.
const AGENT_ATTRIBUTION = require('./scripts/agent-attribution.cjs');

module.exports = {
  extends: ['@commitlint/config-conventional'],
  plugins: [
    {
      rules: {
        'no-agent-attribution': ({ raw }) => [
          !AGENT_ATTRIBUTION.test(raw),
          'agent attribution trailers (Claude-Session:, a Claude co-author) are not allowed',
        ],
      },
    },
  ],
  rules: {
    'no-agent-attribution': [2, 'always'],
  },
  // Dependabot bodies embed release notes and long compare links that blow
  // past body-max-line-length; its commits are machine-generated, so skip.
  ignores: [(message) => message.includes('Signed-off-by: dependabot[bot]')],
};

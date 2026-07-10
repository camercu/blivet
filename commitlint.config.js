module.exports = {
  extends: ['@commitlint/config-conventional'],
  // Dependabot bodies embed release notes and long compare links that blow
  // past body-max-line-length; its commits are machine-generated, so skip.
  ignores: [(message) => message.includes('Signed-off-by: dependabot[bot]')],
};

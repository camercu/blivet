// Fails if any commit reachable from HEAD carries an agent attribution
// trailer. Runs on every push, not only on PRs: the trailers that reached the
// v0.14.0 release notes came in by direct push to main, where the commit-msg
// hook had not run.
//
// A commit that matches but is not attribution can be accepted without
// rewriting history: add its full SHA and a reason to
// agent-attribution-allowlist.txt.
//
// No exemption for dependabot, unlike commitlint's ignores: a message is
// never exempted by text it contains, since an amended or squashed dependabot
// commit keeps its sign-off and can gain a trailer.

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const { PATTERN } = createRequire(import.meta.url)('./agent-attribution.cjs');

const git = (...args) =>
  execFileSync('git', args, { encoding: 'utf8', maxBuffer: 256 * 1024 * 1024 });

// A shallow clone would scan a handful of commits and pass having proven
// nothing.
if (git('rev-parse', '--is-shallow-repository').trim() === 'true') {
  console.error('the clone is shallow; fetch full history (fetch-depth: 0) before scanning');
  process.exit(1);
}

const allowed = new Set(
  readFileSync(new URL('./agent-attribution-allowlist.txt', import.meta.url), 'utf8')
    .split('\n')
    .map((line) => line.replace(/#.*/, '').trim().split(/\s+/)[0])
    .filter(Boolean),
);

const offenders = git('log', '--format=%H %s%x00%B%x1e', 'HEAD')
  .split('\x1e')
  .map((entry) => entry.trim())
  .filter(Boolean)
  .map((entry) => entry.split('\x00'))
  .filter(([title, body]) => !allowed.has(title.split(' ')[0]))
  .filter(([, body]) => PATTERN.test(body))
  .map(([title]) => title);

if (offenders.length > 0) {
  console.error(`${offenders.length} commit(s) carry an agent attribution trailer:`);
  for (const title of offenders) console.error(`  ${title}`);
  process.exit(1);
}
console.log('no agent attribution trailers in history');

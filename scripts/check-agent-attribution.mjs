// Fails if any commit reachable from HEAD carries an agent attribution
// trailer. Runs on every push, not only on PRs: the trailers that reached the
// v0.14.0 release notes came in by direct push to main, where the commit-msg
// hook had not run.

import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';

const AGENT_ATTRIBUTION = createRequire(import.meta.url)('./agent-attribution.cjs');

const log = execFileSync('git', ['log', '--format=%h %s%x00%B%x1e', 'HEAD'], {
  encoding: 'utf8',
  maxBuffer: 256 * 1024 * 1024,
});
const offenders = log
  .split('\x1e')
  .map((entry) => entry.trim())
  .filter(Boolean)
  .map((entry) => entry.split('\x00'))
  .filter(([, body]) => AGENT_ATTRIBUTION.test(body))
  .map(([title]) => title);

if (offenders.length > 0) {
  console.error(`${offenders.length} commit(s) carry an agent attribution trailer:`);
  for (const title of offenders) console.error(`  ${title}`);
  process.exit(1);
}
console.log('no agent attribution trailers in history');

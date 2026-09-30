// What scripts/agent-attribution.cjs must catch and must let through. Run by
// `just check-attribution`, which CI's commitlint job runs.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { test } from 'node:test';

const { PATTERN } = createRequire(import.meta.url)('./agent-attribution.cjs');
const body = (trailer) => `fix: a thing\n\nWhy it changed.\n\n${trailer}\n`;

test('catches the trailers Claude Code writes', () => {
  for (const trailer of [
    'Claude-Session: https://claude.ai/code/session_012JnkN8FDY3vPk5b3CrPiKA',
    'Claude-Session: https://claude.com/code/session_x',
    'Claude-Session: http://claude.ai/code/session_x',
    'Co-Authored-By: Claude <noreply@anthropic.com>',
    'Co-Authored-By: Claude Opus 4.1 <noreply@anthropic.com>',
    'co-authored-by:  Claude <claude@anthropic.com>',
    '🤖 Generated with [Claude Code](https://claude.com/claude-code)',
  ]) {
    assert.ok(PATTERN.test(body(trailer)), trailer);
    assert.ok(PATTERN.test(body(trailer).replaceAll('\n', '\r\n')), `${trailer} (CRLF)`);
  }
});

test('lets prose and human co-authors through', () => {
  for (const line of [
    'Drop the "Generated with Claude" footer from the notes.',
    'Drop the Generated with [Claude Code] footer from the notes.',
    'Co-authored-by: Claude Shannon <claude@example.com>',
    'Claude-Session: wrapping onto a new line in prose',
    'See Claude-Session: https://claude.ai/code/x in the notes.',
    'Strip Co-Authored-By: Claude <noreply@anthropic.com> from the notes.',
    'Co-Authored-By: Claudette <claudette@anthropic.com>',
    'Signed-off-by: dependabot[bot] <support@github.com>',
  ]) {
    assert.ok(!PATTERN.test(body(line)), line);
  }
});

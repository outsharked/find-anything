import { describe, it, expect } from 'vitest';
import { highlightFile } from './highlight';

// FileViewer's paged mode highlights each new page independently (appendCodeState /
// prependCodeState) and concatenates the HTML, instead of re-highlighting the whole
// accumulated buffer on every page load. That optimization only holds if highlightFile
// always emits exactly one '\n'-separated output line per input line — otherwise
// `codeLines` (derived by splitting the concatenated HTML) desyncs from `lineOffsets`
// and line numbers stop lining up with content.

describe('highlightFile', () => {
	it('preserves line count for plain text with no language match', async () => {
		const lines = ['alpha', 'beta', 'gamma'];
		const html = await highlightFile(lines, 'file.unknownext');
		expect(html.split('\n')).toHaveLength(lines.length);
	});

	it('preserves line count when a language is applied', async () => {
		const lines = ['function foo() {', '  return 1;', '}'];
		const html = await highlightFile(lines, 'file.js');
		expect(html.split('\n')).toHaveLength(lines.length);
	});

	it('preserves line count across a multi-line token (block comment)', async () => {
		const lines = ['const a = 1;', '/* a comment', 'spanning lines', '*/', 'const b = 2;'];
		const html = await highlightFile(lines, 'file.js');
		expect(html.split('\n')).toHaveLength(lines.length);
	});

	it('splitting a buffer into two highlighted pages and concatenating preserves total line count', async () => {
		const lines = ['const a = 1;', '// a comment', 'const b = 2;', 'const c = 3;'];
		const full = await highlightFile(lines, 'file.js');

		const page1 = await highlightFile(lines.slice(0, 2), 'file.js');
		const page2 = await highlightFile(lines.slice(2), 'file.js');
		const combined = `${page1}\n${page2}`;

		expect(combined.split('\n')).toHaveLength(full.split('\n').length);
		expect(combined.split('\n')).toHaveLength(lines.length);
	});
});

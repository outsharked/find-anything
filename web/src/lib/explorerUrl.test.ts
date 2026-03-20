import { describe, it, expect } from 'vitest';
import { buildExplorerUrl } from './explorerUrl';

describe('buildExplorerUrl', () => {
	it('uses backslash separator for Windows roots', () => {
		const url = buildExplorerUrl('C:\\Share', 'docs/report.pdf');
		expect(url).toBe('findanything://open?path=' + encodeURIComponent('C:\\Share\\docs\\report.pdf'));
	});

	it('uses forward slash separator for Unix roots', () => {
		const url = buildExplorerUrl('/mnt/nas', 'docs/report.pdf');
		expect(url).toBe('findanything://open?path=' + encodeURIComponent('/mnt/nas/docs/report.pdf'));
	});

	it('strips trailing backslash from Windows root', () => {
		const url = buildExplorerUrl('C:\\Share\\', 'docs/report.pdf');
		expect(url).toBe('findanything://open?path=' + encodeURIComponent('C:\\Share\\docs\\report.pdf'));
	});

	it('strips trailing slash from Unix root', () => {
		const url = buildExplorerUrl('/mnt/nas/', 'docs/report.pdf');
		expect(url).toBe('findanything://open?path=' + encodeURIComponent('/mnt/nas/docs/report.pdf'));
	});

	it('URL-encodes spaces and special characters', () => {
		const url = buildExplorerUrl('C:\\Share', 'my documents/file name.pdf');
		expect(url).toBe(
			'findanything://open?path=' +
				encodeURIComponent('C:\\Share\\my documents\\file name.pdf')
		);
	});

	it('handles nested path segments', () => {
		const url = buildExplorerUrl('/mnt/nas', 'a/b/c/file.txt');
		expect(url).toBe('findanything://open?path=' + encodeURIComponent('/mnt/nas/a/b/c/file.txt'));
	});
});

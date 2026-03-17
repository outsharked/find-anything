import { describe, it, expect } from 'vitest';
import { displayPath, archivePathOf, splitDisplayPath } from './commandPaletteLogic';

// ── displayPath ───────────────────────────────────────────────────────────────

describe('displayPath', () => {
	it('returns the path unchanged for plain files', () => {
		expect(displayPath('src/main.rs')).toBe('src/main.rs');
	});

	it('formats composite paths as "zip → member"', () => {
		expect(displayPath('archive.zip::member.txt')).toBe('archive.zip → member.txt');
	});

	it('handles nested composite paths (only first :: is the separator)', () => {
		expect(displayPath('outer.zip::inner.zip::file.txt')).toBe('outer.zip → inner.zip::file.txt');
	});
});

// ── splitDisplayPath ──────────────────────────────────────────────────────────

describe('splitDisplayPath', () => {
	it('splits plain paths into name and dir', () => {
		expect(splitDisplayPath('/home/user/file.txt')).toEqual({
			name: 'file.txt',
			dir: '/home/user',
		});
	});

	it('returns name with empty dir for a bare filename', () => {
		expect(splitDisplayPath('file.txt')).toEqual({ name: 'file.txt', dir: '' });
	});

	it('uses the inner member as name for a single-level archive member', () => {
		expect(splitDisplayPath('/home/user/archive.zip::member.txt')).toEqual({
			name: 'member.txt',
			dir: '/home/user/archive.zip',
		});
	});

	it('uses the terminal member as name for nested archive members', () => {
		// outer.zip::c.tar::file.txt → name=file.txt, dir=outer.zip::c.tar
		expect(splitDisplayPath('/home/user/outer.zip::c.tar::file.txt')).toEqual({
			name: 'file.txt',
			dir: '/home/user/outer.zip::c.tar',
		});
	});

	it('handles a slash inside the archive member path', () => {
		// archive.zip::subdir/file.txt → last sep is /, name=file.txt
		expect(splitDisplayPath('/home/user/archive.zip::subdir/file.txt')).toEqual({
			name: 'file.txt',
			dir: '/home/user/archive.zip::subdir',
		});
	});
});

// ── archivePathOf ─────────────────────────────────────────────────────────────

describe('archivePathOf', () => {
	it('returns null for plain paths', () => {
		expect(archivePathOf('src/main.rs')).toBeNull();
	});

	it('returns the member portion of a composite path', () => {
		expect(archivePathOf('archive.zip::member.txt')).toBe('member.txt');
	});

	it('returns everything after the first :: for nested paths', () => {
		expect(archivePathOf('outer.zip::inner.zip::file.txt')).toBe('inner.zip::file.txt');
	});
});

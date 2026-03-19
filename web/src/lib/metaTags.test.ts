import { describe, it, expect } from 'vitest';
import { parseMetaTags } from './metaTags';

describe('parseMetaTags', () => {
	it('parses a single tag', () => {
		const tags = parseMetaTags('[AUDIO:codec] MP3');
		expect(tags).toHaveLength(1);
		expect(tags[0]).toMatchObject({ prefix: 'AUDIO', key: 'codec', value: 'MP3', label: 'AUDIO:codec' });
	});

	it('parses multiple space-joined tags', () => {
		const tags = parseMetaTags('[TAG:title] Song Title [TAG:artist] John Doe [AUDIO:codec] MP3');
		expect(tags).toHaveLength(3);
		expect(tags[0]).toMatchObject({ label: 'TAG:title', value: 'Song Title' });
		expect(tags[1]).toMatchObject({ label: 'TAG:artist', value: 'John Doe' });
		expect(tags[2]).toMatchObject({ label: 'AUDIO:codec', value: 'MP3' });
	});

	it('preserves spaces inside values', () => {
		const tags = parseMetaTags('[EXIF:Make] Apple Inc [EXIF:Model] iPhone 15 Pro');
		expect(tags[0].value).toBe('Apple Inc');
		expect(tags[1].value).toBe('iPhone 15 Pro');
	});

	it('handles square brackets inside a value', () => {
		const tags = parseMetaTags('[TAG:title] Hello [World] [TAG:artist] Jane');
		expect(tags).toHaveLength(2);
		expect(tags[0].value).toBe('Hello [World]');
		expect(tags[1].value).toBe('Jane');
	});

	it('handles square brackets with colon inside a value', () => {
		// Lower-case content in brackets should not be treated as a tag delimiter.
		const tags = parseMetaTags('[TAG:title] re: [something] here [TAG:album] Album');
		expect(tags).toHaveLength(2);
		expect(tags[0].value).toBe('re: [something] here');
		expect(tags[1].value).toBe('Album');
	});

	it('returns empty array for empty string', () => {
		expect(parseMetaTags('')).toHaveLength(0);
	});

	it('returns empty array for string with no tags', () => {
		expect(parseMetaTags('no tags here')).toHaveLength(0);
	});

	it('parses EXIF dimension tag correctly', () => {
		const tags = parseMetaTags('[IMAGE:dimensions] 1920x1080 [IMAGE:bit_depth] 24');
		expect(tags[0]).toMatchObject({ label: 'IMAGE:dimensions', value: '1920x1080' });
		expect(tags[1]).toMatchObject({ label: 'IMAGE:bit_depth', value: '24' });
	});
});

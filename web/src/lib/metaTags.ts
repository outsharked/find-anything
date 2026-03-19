/** A single parsed metadata tag from a space-joined metadata line. */
export interface MetaTag {
	/** Prefix before the colon, e.g. "EXIF", "TAG", "AUDIO", "IMAGE", "VIDEO". */
	prefix: string;
	/** Key after the colon, e.g. "Make", "title", "codec". */
	key: string;
	/** Display value, e.g. "Apple", "Song Title", "MP3". */
	value: string;
	/** Full label inside brackets, e.g. "EXIF:Make". */
	label: string;
}

/**
 * Parse a space-joined metadata content string into individual tag entries.
 *
 * Metadata is stored as a single string with tags concatenated by spaces:
 *   "[EXIF:Make] Apple [EXIF:Model] iPhone 13 [AUDIO:codec] MP3"
 *
 * Tag labels always follow the pattern [UPPERCASE:key].  Values run until the
 * next tag delimiter or end of string.  Bracket characters in values are safe
 * as long as they don't look like [UPPERCASE: (i.e. the lookahead only breaks
 * at our specific tag format, not at arbitrary square brackets).
 */
export function parseMetaTags(content: string): MetaTag[] {
	const tags: MetaTag[] = [];
	// Label must start with one or more uppercase ASCII letters followed by ':'.
	// Value runs until the next such tag opener or end of string.
	const re = /\[([A-Z]+:[^\]]*)\]\s*((?:(?!\[[A-Z]+:).)*)/g;
	let m: RegExpExecArray | null;
	while ((m = re.exec(content)) !== null) {
		const label = m[1];
		const value = m[2].trim();
		const colonIdx = label.indexOf(':');
		const prefix = label.slice(0, colonIdx);
		const key = label.slice(colonIdx + 1);
		tags.push({ prefix, key, value, label });
	}
	return tags;
}

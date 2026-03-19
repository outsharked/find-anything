import { parseMetaTags } from './metaTags';

/**
 * Parse image pixel dimensions from indexed metadata lines.
 *
 * Three tag families are recognised, in priority order:
 *  1. [EXIF:PixelXDimension] / [EXIF:PixelYDimension]  — actual rendered size (Exif 2.x)
 *  2. [EXIF:ImageWidth]      / [EXIF:ImageLength]       — TIFF-style dimensions
 *  3. [IMAGE:dimensions] WxH                            — our own basic-extractor fallback
 *
 * Returns null when no usable dimensions are found.
 */
export function parseImageDimensions(
	lines: { content: string }[]
): { width: number; height: number } | null {
	const allTags = lines.flatMap(l => parseMetaTags(l.content));
	const tagMap = new Map(allTags.map(t => [t.label, t.value]));
	const getInt = (k: string) => { const v = tagMap.get(k); return v !== undefined ? parseInt(v) : null; };

	const pixelW = getInt('EXIF:PixelXDimension');
	const pixelH = getInt('EXIF:PixelYDimension');
	const imageW = getInt('EXIF:ImageWidth');
	const imageH = getInt('EXIF:ImageLength');

	const dimStr = tagMap.get('IMAGE:dimensions');
	const dimMatch = dimStr?.match(/^(\d+)x(\d+)/);
	const basicW = dimMatch ? parseInt(dimMatch[1]) : null;
	const basicH = dimMatch ? parseInt(dimMatch[2]) : null;

	const w = pixelW ?? imageW ?? basicW;
	const h = pixelH ?? imageH ?? basicH;
	return w && h ? { width: w, height: h } : null;
}

import { describe, it, expect } from 'vitest';
import { parseGoToLineInput } from './goToLine';

describe('parseGoToLineInput', () => {
	it('plain number "5000" → 5000', () => {
		expect(parseGoToLineInput('5000')).toBe(5000);
	});

	it('trims surrounding whitespace', () => {
		expect(parseGoToLineInput('  42  ')).toBe(42);
	});

	it('accepts "L500" / "l500" / "#L500" forms', () => {
		expect(parseGoToLineInput('L500')).toBe(500);
		expect(parseGoToLineInput('l500')).toBe(500);
		expect(parseGoToLineInput('#L500')).toBe(500);
	});

	it('rejects zero and negative numbers', () => {
		expect(parseGoToLineInput('0')).toBeNull();
		expect(parseGoToLineInput('-5')).toBeNull();
	});

	it('rejects empty input', () => {
		expect(parseGoToLineInput('')).toBeNull();
		expect(parseGoToLineInput('   ')).toBeNull();
	});

	it('rejects non-numeric garbage', () => {
		expect(parseGoToLineInput('abc')).toBeNull();
		expect(parseGoToLineInput('12abc')).toBeNull();
		expect(parseGoToLineInput('L-5')).toBeNull();
	});

	it('rejects decimals', () => {
		expect(parseGoToLineInput('5.5')).toBeNull();
	});
});

import { describe, it, expect } from 'vitest';
import { validateInviteName, parseDurationSecs, formatTimestamp, SCOPE_OPTIONS } from './adminTokensLogic';

describe('SCOPE_OPTIONS', () => {
	it('covers all three scopes in hierarchical order', () => {
		expect(SCOPE_OPTIONS.map((o) => o.value)).toEqual(['read', 'update-index', 'admin']);
	});
});

describe('validateInviteName', () => {
	it('rejects empty or whitespace-only names', () => {
		expect(validateInviteName('')).toMatch(/required/i);
		expect(validateInviteName('   ')).toMatch(/required/i);
	});
	it('rejects control characters', () => {
		expect(validateInviteName('bad\nname')).toMatch(/control/i);
		expect(validateInviteName('bad\tname')).toMatch(/control/i);
	});
	it('accepts a normal name', () => {
		expect(validateInviteName('synology1-scanner')).toBeNull();
		expect(validateInviteName('  laptop  ')).toBeNull();
	});
});

describe('parseDurationSecs', () => {
	it('treats blank input as "use server default"', () => {
		expect(parseDurationSecs('')).toBeUndefined();
		expect(parseDurationSecs('   ')).toBeUndefined();
	});
	it('parses bare seconds and suffixed units', () => {
		expect(parseDurationSecs('90')).toBe(90);
		expect(parseDurationSecs('30s')).toBe(30);
		expect(parseDurationSecs('15m')).toBe(900);
		expect(parseDurationSecs('2h')).toBe(7200);
		expect(parseDurationSecs('7d')).toBe(604_800);
		expect(parseDurationSecs('2w')).toBe(1_209_600);
	});
	it('tolerates surrounding and inner whitespace', () => {
		expect(parseDurationSecs('  15m  ')).toBe(900);
		expect(parseDurationSecs('15 m')).toBe(900);
	});
	it('rejects invalid input', () => {
		expect(parseDurationSecs('abc')).toBeNull();
		expect(parseDurationSecs('m')).toBeNull();
		expect(parseDurationSecs('-5m')).toBeNull();
		expect(parseDurationSecs('0m')).toBeNull();
		expect(parseDurationSecs('99999999999999999999d')).toBeNull();
	});
});

describe('formatTimestamp', () => {
	it('shows "never" for null or undefined', () => {
		expect(formatTimestamp(null)).toBe('never');
		expect(formatTimestamp(undefined)).toBe('never');
	});
	it('formats a real timestamp as a date', () => {
		const out = formatTimestamp(0);
		expect(out).not.toBe('never');
		expect(out.length).toBeGreaterThan(0);
	});
});

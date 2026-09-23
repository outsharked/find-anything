import { describe, it, expect } from 'vitest';
import { normalizeInviteCode, looksLikeInviteCode, planConnect, redeemErrorMessage } from './authLogic';

describe('normalizeInviteCode', () => {
	it('uppercases and strips dashes and whitespace', () => {
		expect(normalizeInviteCode(' abcd-efgh ')).toBe('ABCDEFGH');
	});
	it('maps Crockford confusables', () => {
		expect(normalizeInviteCode('o1il-0000')).toBe('01110000');
	});
});

describe('looksLikeInviteCode', () => {
	it('accepts the displayed form, lowercase, and dashless form', () => {
		expect(looksLikeInviteCode('ABCD-EFGH')).toBe(true);
		expect(looksLikeInviteCode('abcd-efgh')).toBe(true);
		expect(looksLikeInviteCode('abcdefgh')).toBe(true);
		expect(looksLikeInviteCode('  7K3M-9P2Q\n')).toBe(true);
	});
	it('rejects generated tokens and the fa_ prefix', () => {
		expect(looksLikeInviteCode('fa_' + 'a'.repeat(64))).toBe(false);
		expect(looksLikeInviteCode('fa_abcde')).toBe(false);
	});
	it('rejects base64 root tokens (too long / non-alphanumeric)', () => {
		expect(looksLikeInviteCode('q3JkZm9vYmFyYmF6cXV4MTIzNDU2Nzg5MGFiY2Q=')).toBe(false);
	});
	it('rejects wrong lengths and empty input', () => {
		expect(looksLikeInviteCode('')).toBe(false);
		expect(looksLikeInviteCode('ABC-DEF')).toBe(false);
		expect(looksLikeInviteCode('ABCD-EFGH-J')).toBe(false);
	});
});

describe('planConnect', () => {
	it('chooses invite for code-shaped input, token otherwise', () => {
		expect(planConnect(' ABCD-EFGH ')).toEqual({ kind: 'invite', code: 'ABCD-EFGH' });
		expect(planConnect(' some-long-static-token ')).toEqual({ kind: 'token', token: 'some-long-static-token' });
	});
});

describe('redeemErrorMessage', () => {
	it('explains known statuses', () => {
		expect(redeemErrorMessage(401)).toMatch(/invalid, expired/i);
		expect(redeemErrorMessage(429)).toMatch(/too many/i);
		expect(redeemErrorMessage(409)).toMatch(/already exists/i);
		expect(redeemErrorMessage(500)).toContain('500');
	});
});

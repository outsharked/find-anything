/**
 * Pure helpers for the "connect to server" dialog (plan 094).
 *
 * The dialog accepts either a one-time invite code (`ABCD-EFGH`, produced by
 * `find-admin invite create`) or a raw bearer token. Invite codes are exactly
 * 8 characters once dashes/whitespace are removed; generated tokens are far
 * longer, so length is what tells them apart.
 */

/** Canonicalise user input the way the server does: uppercase, drop dashes
 *  and whitespace, map the Crockford confusables (I/L → 1, O → 0). */
export function normalizeInviteCode(input: string): string {
	return input
		.replace(/[\s-]/g, '')
		.toUpperCase()
		.replace(/[IL]/g, '1')
		.replace(/O/g, '0');
}

/** True when the input has the shape of an invite code. */
export function looksLikeInviteCode(input: string): boolean {
	const t = input.trim();
	if (t.startsWith('fa_')) return false;
	return /^[0-9A-Z]{8}$/.test(normalizeInviteCode(t));
}

export type ConnectPlan =
	| { kind: 'invite'; code: string }
	| { kind: 'token'; token: string };

export function planConnect(input: string): ConnectPlan {
	const t = input.trim();
	return looksLikeInviteCode(t) ? { kind: 'invite', code: t } : { kind: 'token', token: t };
}

/** Human-readable message for a failed redemption. */
export function redeemErrorMessage(status: number): string {
	switch (status) {
		case 401: return 'Invalid, expired or already-used invite code.';
		case 429: return 'Too many failed attempts. Wait a minute and try again.';
		case 409: return 'A token with this invite’s name already exists. Ask the admin for a new invite.';
		default:  return `Could not redeem the invite (server returned ${status}).`;
	}
}

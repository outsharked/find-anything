<script lang="ts">
	import { onMount } from 'svelte';
	import {
		getInboxStatus, retryFailedInbox,
		listInvites, createInvite, revokeInvite,
		listTokens, revokeToken,
		ForbiddenError
	} from '$lib/api';
	import type { InboxStatusResponse, InviteInfo, TokenInfo, Scope } from '$lib/api';
	import {
		SCOPE_OPTIONS, validateInviteName, parseDurationSecs, formatTimestamp
	} from '$lib/adminTokensLogic';

	let inbox: InboxStatusResponse | null = $state(null);
	let invites: InviteInfo[] | null = $state(null);
	let tokens: TokenInfo[] | null = $state(null);

	let loading = $state(true);
	let forbidden = $state(false);
	let error: string | null = $state(null);
	let retrying = $state(false);
	let retryMessage: string | null = $state(null);

	// ── Create-invite form ──────────────────────────────────────────────────────
	let inviteName = $state('');
	let inviteScope: Scope = $state('update-index');
	let inviteTtl = $state('');
	let tokenTtl = $state('');
	let createError: string | null = $state(null);
	let creating = $state(false);
	let justCreated: { code: string; name: string; scope: Scope; expiresAt: number } | null = $state(null);
	let copied = $state(false);

	onMount(() => {
		loadAll();
	});

	async function loadAll() {
		error = null;
		forbidden = false;
		try {
			const [i, inv, tok] = await Promise.all([getInboxStatus(), listInvites(), listTokens()]);
			inbox = i;
			invites = inv;
			tokens = tok;
		} catch (e) {
			if (e instanceof ForbiddenError) {
				forbidden = true;
			} else {
				error = String(e);
			}
		} finally {
			loading = false;
		}
	}

	async function refreshInvitesAndTokens() {
		try {
			[invites, tokens] = await Promise.all([listInvites(), listTokens()]);
		} catch (e) {
			error = String(e);
		}
	}

	async function handleRetry() {
		retrying = true;
		retryMessage = null;
		try {
			const result = await retryFailedInbox();
			retryMessage = `Queued ${result.retried} item${result.retried === 1 ? '' : 's'} for reprocessing.`;
			inbox = await getInboxStatus();
		} catch (e) {
			error = String(e);
		} finally {
			retrying = false;
		}
	}

	async function handleCreateInvite() {
		createError = null;
		justCreated = null;

		const nameError = validateInviteName(inviteName);
		if (nameError) { createError = nameError; return; }

		const ttlSecs = parseDurationSecs(inviteTtl);
		if (ttlSecs === null) { createError = 'Invalid invite lifetime — use e.g. 15m, 2h, 7d.'; return; }
		const tokenTtlSecs = parseDurationSecs(tokenTtl);
		if (tokenTtlSecs === null) { createError = 'Invalid token lifetime — use e.g. 30d, 6h.'; return; }

		creating = true;
		try {
			const name = inviteName.trim();
			const resp = await createInvite({
				name,
				scope: inviteScope,
				ttl_secs: ttlSecs,
				token_ttl_secs: tokenTtlSecs
			});
			justCreated = { code: resp.code, name, scope: inviteScope, expiresAt: resp.expires_at };
			inviteName = '';
			inviteTtl = '';
			tokenTtl = '';
			await refreshInvitesAndTokens();
		} catch (e) {
			createError = e instanceof ForbiddenError
				? 'This token cannot create invites (needs admin scope).'
				: String(e);
		} finally {
			creating = false;
		}
	}

	async function handleRevokeInvite(inv: InviteInfo) {
		if (!confirm(`Revoke the pending invite for '${inv.name}'? The code will stop working immediately.`)) return;
		try {
			await revokeInvite(inv.id);
			await refreshInvitesAndTokens();
		} catch (e) {
			error = String(e);
		}
	}

	async function handleRevokeToken(tok: TokenInfo) {
		if (!confirm(`Revoke the token '${tok.name}'? Anything using it will lose access immediately.`)) return;
		try {
			await revokeToken(tok.name);
			await refreshInvitesAndTokens();
		} catch (e) {
			error = String(e);
		}
	}

	async function copyCode() {
		if (!justCreated) return;
		try {
			await navigator.clipboard.writeText(justCreated.code);
			copied = true;
			setTimeout(() => (copied = false), 1500);
		} catch {
			/* Clipboard API unavailable (e.g. insecure context) — the code is
			   still visible on screen to copy by hand. */
		}
	}
</script>

{#if loading}
	<p class="muted">Loading…</p>
{:else if forbidden}
	<p class="muted">
		This token doesn't have admin access. Ask an admin to create an
		<code>admin</code>-scoped invite for you.
	</p>
{:else if error}
	<p class="error">{error}</p>
{:else}
	{#if inbox}
		<div class="section">
			<h3 class="section-title">Inbox</h3>
			<div class="row">
				<span class="label">Pending</span>
				<span class="value">{inbox.pending.length}</span>
			</div>
			<div class="row">
				<span class="label">Failed</span>
				<span class="value" class:warn={inbox.failed.length > 0}>{inbox.failed.length}</span>
			</div>
			{#if inbox.failed.length > 0}
				<div class="actions">
					<button class="btn" onclick={handleRetry} disabled={retrying}>
						{retrying ? 'Retrying…' : 'Retry Failed'}
					</button>
				</div>
			{/if}
			{#if retryMessage}
				<p class="success">{retryMessage}</p>
			{/if}
		</div>
	{/if}

	<div class="section">
		<h3 class="section-title">Invite a New Client</h3>
		<p class="muted">
			Creates a one-time code that a new client redeems for its own named,
			scoped token — the code itself is never reusable and expires quickly.
		</p>
		<label class="field">
			<span class="field-label">Name</span>
			<input
				type="text"
				bind:value={inviteName}
				placeholder="e.g. laptop-browser, synology1"
				onkeydown={(e) => e.key === 'Enter' && handleCreateInvite()}
			/>
		</label>
		<label class="field">
			<span class="field-label">Scope</span>
			<select bind:value={inviteScope}>
				{#each SCOPE_OPTIONS as opt (opt.value)}
					<option value={opt.value}>{opt.label} — {opt.hint}</option>
				{/each}
			</select>
		</label>
		<div class="field-row">
			<label class="field">
				<span class="field-label">Invite valid for</span>
				<input type="text" bind:value={inviteTtl} placeholder="15m (default)" />
			</label>
			<label class="field">
				<span class="field-label">Token expires in</span>
				<input type="text" bind:value={tokenTtl} placeholder="never (default)" />
			</label>
		</div>
		{#if createError}<p class="error">{createError}</p>{/if}
		<div class="actions">
			<button class="btn" onclick={handleCreateInvite} disabled={creating}>
				{creating ? 'Creating…' : 'Create Invite'}
			</button>
		</div>
		{#if justCreated}
			<div class="invite-result">
				<p class="muted">
					Code for <strong>{justCreated.name}</strong> ({justCreated.scope}) —
					shown once, valid until {formatTimestamp(justCreated.expiresAt)}:
				</p>
				<div class="code-row">
					<code class="code">{justCreated.code}</code>
					<button class="btn btn-small" onclick={copyCode}>{copied ? 'Copied!' : 'Copy'}</button>
				</div>
			</div>
		{/if}
	</div>

	<div class="section">
		<h3 class="section-title">Pending Invites</h3>
		{#if invites && invites.length === 0}
			<p class="muted">None.</p>
		{:else if invites}
			{#each invites as inv (inv.id)}
				<div class="row">
					<span class="label">{inv.name} <span class="scope-badge">{inv.scope}</span></span>
					<span class="value">
						expires {formatTimestamp(inv.expires_at)}
						<button class="btn btn-small" onclick={() => handleRevokeInvite(inv)}>Revoke</button>
					</span>
				</div>
			{/each}
		{/if}
	</div>

	<div class="section">
		<h3 class="section-title">Access Tokens</h3>
		<p class="muted">
			The root token from <code>server.toml</code> is not listed here and
			cannot be revoked from this panel.
		</p>
		{#if tokens && tokens.length === 0}
			<p class="muted">None yet — create an invite above to enroll the first client.</p>
		{:else if tokens}
			{#each tokens as tok (tok.name)}
				<div class="row">
					<span class="label">{tok.name} <span class="scope-badge">{tok.scope}</span></span>
					<span class="value">
						last used {formatTimestamp(tok.last_used_at)}
						<button class="btn btn-small" onclick={() => handleRevokeToken(tok)}>Revoke</button>
					</span>
				</div>
			{/each}
		{/if}
	</div>
{/if}

<style>
	.section {
		display: flex;
		flex-direction: column;
		gap: 8px;
		max-width: 480px;
		margin-bottom: 28px;
	}

	.section-title {
		font-size: 13px;
		font-weight: 600;
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.05em;
		margin: 0 0 4px;
	}

	.row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 8px;
		padding: 6px 0;
		border-bottom: 1px solid var(--border);
		font-size: 13px;
	}

	.label {
		color: var(--text-muted);
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.value {
		color: var(--text);
		font-variant-numeric: tabular-nums;
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.value.warn {
		color: var(--accent-warn, #e3a140);
		font-weight: 600;
	}

	.scope-badge {
		font-size: 11px;
		padding: 1px 6px;
		border-radius: 3px;
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		color: var(--text-muted);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 3px;
		font-size: 13px;
	}

	.field-label {
		color: var(--text-muted);
	}

	.field-row {
		display: flex;
		gap: 12px;
	}

	.field-row .field {
		flex: 1;
		min-width: 0;
	}

	input[type='text'],
	select {
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		color: var(--text);
		font-size: 13px;
		padding: 6px 8px;
		border-radius: 4px;
	}

	input[type='text']:focus,
	select:focus {
		outline: 1px solid var(--accent, #58a6ff);
	}

	.invite-result {
		margin-top: 8px;
		padding: 10px 12px;
		border: 1px solid var(--border);
		border-radius: 6px;
		background: var(--bg-secondary);
	}

	.code-row {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.code {
		font-family: var(--mono-font, monospace);
		font-size: 15px;
		font-weight: 600;
		letter-spacing: 0.05em;
		color: var(--text);
	}

	.actions {
		margin-top: 8px;
	}

	.btn {
		background: var(--bg-secondary);
		border: 1px solid var(--border);
		color: var(--text);
		font-size: 13px;
		padding: 6px 14px;
		border-radius: 4px;
		cursor: pointer;
	}

	.btn-small {
		padding: 3px 10px;
		font-size: 12px;
	}

	.btn:hover:not(:disabled) {
		background: var(--bg-hover, rgba(255, 255, 255, 0.08));
	}

	.btn:disabled {
		opacity: 0.5;
		cursor: default;
	}

	.muted {
		color: var(--text-muted);
		font-size: 13px;
	}

	.error {
		color: var(--accent-error, #f85149);
		font-size: 13px;
	}

	.success {
		color: var(--accent-success, #3fb950);
		font-size: 13px;
	}
</style>

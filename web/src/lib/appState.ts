import { FilePath } from './filePath';
import type { LineSelection } from './lineSelection';
import { parseHash } from './lineSelection';

export type View = 'results' | 'file';
export type PanelMode = 'file' | 'dir';

/** All state needed to render the file/directory viewer panel.
 *  Bundled into one object so event handlers set all fields atomically. */
export interface FileViewState {
	source: string;
	file: FilePath;
	selection: LineSelection;
	panelMode: PanelMode;
	/** Only meaningful when panelMode === 'dir'. */
	dirPrefix: string;
}

export interface AppState {
	view: View;
	query: string;
	mode: string;
	selectedSources: string[];
	fileSource: string;
	currentFile: FilePath | null;
	fileSelection: LineSelection;
	panelMode: PanelMode;
	currentDirPrefix: string;
}

// Serializable form stored in history.state via SvelteKit's pushState/replaceState.
// FilePath class instances don't survive structured clone (prototype getters are lost),
// so currentFile is stored as its string path and reconstructed on restore.
export type SerializedAppState = Omit<AppState, 'currentFile'> & {
	currentFilePath: string | null;
};

export function serializeState(s: AppState): SerializedAppState {
	const { currentFile, ...rest } = s;
	return { ...rest, currentFilePath: currentFile?.full ?? null };
}

export function deserializeState(s: SerializedAppState): AppState {
	return { ...s, currentFile: s.currentFilePath ? new FilePath(s.currentFilePath) : null };
}

export function buildUrl(s: AppState): string {
	const p = new URLSearchParams();
	if (s.query) p.set('q', s.query);
	if (s.mode && s.mode !== 'fuzzy') p.set('mode', s.mode);
	s.selectedSources.forEach((src) => p.append('source', src));
	if (s.view === 'file' && s.currentFile) {
		p.set('view', 'file');
		p.set('fsource', s.fileSource);
		p.set('path', s.currentFile.outer);
		if (s.currentFile.inner) p.set('apath', s.currentFile.inner);
		if (s.panelMode === 'dir') {
			p.set('panel', 'dir');
			p.set('dir', s.currentDirPrefix);
		}
	}
	const qs = p.toString();
	return qs ? `?${qs}` : location.pathname;
}

/** Expand a FileViewState into the flat AppState fields. */
export function expandFileView(fv: FileViewState | null): Pick<AppState, 'view' | 'fileSource' | 'currentFile' | 'fileSelection' | 'panelMode' | 'currentDirPrefix'> {
	if (!fv) {
		return { view: 'results', fileSource: '', currentFile: null, fileSelection: [], panelMode: 'file', currentDirPrefix: '' };
	}
	return { view: 'file', fileSource: fv.source, currentFile: fv.file, fileSelection: fv.selection, panelMode: fv.panelMode, currentDirPrefix: fv.dirPrefix };
}

/** Assemble a FileViewState from AppState fields (returns null for 'results' view). */
export function collapseFileView(s: AppState): FileViewState | null {
	if (s.view !== 'file' || !s.currentFile) return null;
	return { source: s.fileSource, file: s.currentFile, selection: s.fileSelection, panelMode: s.panelMode, dirPrefix: s.currentDirPrefix };
}

export function restoreFromParams(
	params: URLSearchParams
): AppState & { showTree: boolean } {
	const v = (params.get('view') ?? 'results') as View;
	const path = params.get('path');
	const apath = params.get('apath');
	return {
		view: v,
		query: params.get('q') ?? '',
		mode: params.get('mode') ?? 'fuzzy',
		selectedSources: params.getAll('source'),
		fileSource: params.get('fsource') ?? '',
		currentFile: path ? FilePath.fromParts(path, apath) : null,
		fileSelection: parseHash(location.hash),
		panelMode: (params.get('panel') ?? 'file') as PanelMode,
		currentDirPrefix: params.get('dir') ?? '',
		showTree: v === 'file',
	};
}

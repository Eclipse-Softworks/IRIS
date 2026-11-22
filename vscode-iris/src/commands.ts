import * as vscode from 'vscode';
import * as child_process from 'child_process';
import * as path from 'path';
import * as fs from 'fs';
import { getIrisVersionInfo, clearVersionCache } from './statusBar';

export const runDiagCollection = vscode.languages.createDiagnosticCollection('iris-run');
let lastEmitContent = '';
let lastEmitLanguage = 'plaintext';

export class IrisEmitProvider implements vscode.TextDocumentContentProvider {
    provideTextDocumentContent(_uri: vscode.Uri): string {
        return lastEmitContent;
    }
}

export async function runIrisFile(
    _context: vscode.ExtensionContext,
    subcommand: 'run' | 'build',
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel,
    forceGui?: boolean,
): Promise<void> {
    const editor = vscode.window.activeTextEditor;
    if (!editor) {
        vscode.window.showWarningMessage('No active .iris file.');
        return;
    }
    if (!editor.document.fileName.endsWith('.iris')) {
        vscode.window.showWarningMessage('Active file is not an .iris file.');
        return;
    }
    // Await the save: otherwise the compiler can read the previous contents.
    await editor.document.save();
    runFileAtPath(editor.document.fileName, subcommand, getIrisExe, outputChannel, forceGui);
}

export async function runNamedFunction(
    uriStr: string,
    _fnName: string,
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel
): Promise<void> {
    const filePath = vscode.Uri.parse(uriStr).fsPath;
    const doc = vscode.workspace.textDocuments.find(d => d.uri.fsPath === filePath);
    if (doc) { await doc.save(); }
    runFileAtPath(filePath, 'run', getIrisExe, outputChannel);
}

export function runFileAtPath(
    filePath: string,
    subcommand: 'run' | 'build',
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel,
    forceGui?: boolean,
): void {
    const exe = getIrisExe();
    const showTiming = vscode.workspace.getConfiguration('iris').get<boolean>('showTimingOnRun', true);
    outputChannel.clear();
    outputChannel.show(true);
    outputChannel.appendLine(`$ iris ${subcommand} "${path.basename(filePath)}"`);
    outputChannel.appendLine('');

    const args = buildRunArgs(filePath, subcommand, vscode.workspace.getConfiguration('iris'), forceGui);

    const startTime = Date.now();

    // No shell: arguments are passed through verbatim, so paths containing
    // spaces or shell metacharacters need no quoting and cannot be re-parsed.
    const proc = child_process.spawn(exe, args, {
        shell: false,
        windowsHide: true,
        cwd: path.dirname(filePath),
    });

    proc.stdout.on('data', (data: Buffer) => {
        outputChannel.append(data.toString());
    });
    proc.stderr.on('data', (data: Buffer) => {
        const text = data.toString();
        outputChannel.append(text);
        parseAndShowErrors(text, filePath);
    });
    proc.on('close', (code: number | null) => {
        const elapsed = Date.now() - startTime;
        outputChannel.appendLine('');
        if (code === 0) {
            const timingStr = showTiming ? ` in ${elapsed}ms` : '';
            outputChannel.appendLine(`✓ Done (exit 0)${timingStr}`);
            runDiagCollection.delete(vscode.Uri.file(filePath));
        } else {
            outputChannel.appendLine(`✗ Failed (exit ${code})`);
            vscode.window.showErrorMessage(
                `IRIS ${subcommand} failed — check the output panel for details.`,
                'Show Output',
            ).then(choice => {
                if (choice === 'Show Output') { outputChannel.show(); }
            });
        }
    });
    proc.on('error', (err: Error) => {
        outputChannel.appendLine(`Error: ${err.message}`);
        vscode.window.showErrorMessage(
            `IRIS: Cannot run '${exe}'. Is it installed? Set iris.executablePath in settings.`,
            'Open Settings',
        ).then(choice => {
            if (choice === 'Open Settings') {
                vscode.commands.executeCommand('workbench.action.openSettings', 'iris.executablePath');
            }
        });
    });
}

/**
 * Command line for `iris run` / `iris build`.
 *
 * Every option goes *before* the subcommand. The compiler's `--target` and
 * `-o` are top-level options only (subcommands such as `embedded` define
 * their own), and IRIS 1.0.0-rc2 and earlier accepted none of the
 * options after the subcommand -- placing them after `build` made the build
 * fail whenever `iris.target`, `iris.sandbox` or `iris.noCache` was set.
 */
export function buildRunArgs(
    filePath: string,
    subcommand: 'run' | 'build',
    cfg: { get<T>(key: string, defaultValue: T): T },
    forceGui?: boolean,
): string[] {
    const args: string[] = [];
    if (cfg.get<boolean>('sandbox', false)) {
        args.push('--sandbox');
    }
    if (cfg.get<boolean>('noCache', false)) {
        args.push('--no-cache');
    }
    if (cfg.get<boolean>('strictEffects', false)) {
        args.push('--strict-effects');
    }
    if (cfg.get<boolean>('simd', false)) {
        args.push('--simd');
    }
    const gpu = cfg.get<string>('gpu', '').trim();
    if (gpu) {
        args.push('--gpu', gpu);
    }
    if (subcommand === 'build') {
        // --target only affects llvm/binary outputs, so it is a build-only flag.
        const target = cfg.get<string>('target', '').trim();
        if (target) {
            args.push('--target', target);
        }
        // Debug info applies to host builds only; the compiler rejects -g with --target.
        if (cfg.get<boolean>('build.debugInfo', false) && !target) {
            args.push('-g');
        }
        args.push(subcommand);
        if (forceGui ?? cfg.get<boolean>('build.gui', false)) {
            args.push('--gui');
        }
        const outBinary = filePath.replace(/\.iris$/, '') + (process.platform === 'win32' ? '.exe' : '');
        args.push('-o', outBinary, filePath);
        return args;
    }
    args.push(subcommand, filePath);
    return args;
}

export async function debugIrisFile(
    target: vscode.Uri | string | undefined,
    getIrisExe: () => string,
    forceNative?: boolean,
): Promise<void> {
    let filePath: string | undefined;

    if (target instanceof vscode.Uri) {
        filePath = target.fsPath;
    } else if (typeof target === 'string') {
        filePath = vscode.Uri.parse(target).fsPath;
    } else {
        const editor = vscode.window.activeTextEditor;
        filePath = editor?.document.fileName;
    }

    if (!filePath || !filePath.endsWith('.iris')) {
        vscode.window.showWarningMessage('Open an .iris file first.');
        return;
    }

    const doc = vscode.workspace.textDocuments.find(d => d.uri.fsPath === filePath);
    if (doc) { await doc.save(); }

    const cfg = vscode.workspace.getConfiguration('iris');
    const stopOnEntry = cfg.get<boolean>('debug.stopOnEntry', false);
    const native = forceNative ?? cfg.get<boolean>('debug.native', false);
    const uri = vscode.Uri.file(filePath);

    await vscode.debug.startDebugging(vscode.workspace.getWorkspaceFolder(uri), {
        type: 'iris',
        request: 'launch',
        name: `Debug ${path.basename(filePath)}${native ? ' (native)' : ''}`,
        program: filePath,
        stopOnEntry,
        native,
    });
}

export function openRepl(
    _context: vscode.ExtensionContext,
    getIrisExe: () => string
): void {
    const exe = getIrisExe();
    const existing = vscode.window.terminals.find(t => t.name === 'IRIS REPL');
    if (existing) {
        existing.show();
        return;
    }
    const terminal = vscode.window.createTerminal({
        name: 'IRIS REPL',
        shellPath: exe,
        shellArgs: ['repl'],
    });
    terminal.show();
}

/** Emit kinds worth showing in the editor, i.e. the ones producing text. */
export const TEXT_EMIT_KINDS = [
    'ir',
    'llvm',
    'llvm-complete',
    'cuda',
    'cuda-ptx',
    'simd',
    'graph',
    'onnx',
    'tensorrt',
    'pgo-instrument',
    'pgo-optimize',
] as const;

export type EmitKind = (typeof TEXT_EMIT_KINDS)[number];

/** Syntax to display each emit kind with. */
function emitLanguageId(kind: EmitKind): string {
    switch (kind) {
        case 'llvm':
        case 'llvm-complete':
        case 'cuda':
        case 'simd':
        case 'pgo-instrument':
        case 'pgo-optimize':
            return 'llvm';
        case 'onnx':
        case 'graph':
            return 'json';
        default:
            return 'plaintext';
    }
}

export async function showEmit(
    kind: EmitKind,
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel
): Promise<void> {
    const editor = vscode.window.activeTextEditor;
    if (!editor || !editor.document.fileName.endsWith('.iris')) {
        vscode.window.showWarningMessage('Open an .iris file first.');
        return;
    }
    await editor.document.save();
    const exe = getIrisExe();
    const filePath = editor.document.fileName;
    try {
        const out = child_process.execFileSync(exe, ['--emit', kind, filePath], {
            encoding: 'utf8',
            timeout: 60000,
            windowsHide: true,
        });
        lastEmitContent = out;
        lastEmitLanguage = emitLanguageId(kind);
        const uri = vscode.Uri.parse(`iris-emit://output/${path.basename(filePath)}.${kind}`);
        const doc = await vscode.workspace.openTextDocument(uri);
        await vscode.languages.setTextDocumentLanguage(doc, lastEmitLanguage);
        await vscode.window.showTextDocument(doc, vscode.ViewColumn.Beside, true);
    } catch (err: any) {
        // The compiler writes diagnostics to stderr; surface them rather than
        // just the spawn error, which on its own says nothing useful.
        const stderr = err?.stderr?.toString?.() ?? '';
        outputChannel.appendLine(`iris --emit ${kind} failed: ${err.message || err}`);
        if (stderr) {
            outputChannel.appendLine(stderr);
            parseAndShowErrors(stderr, filePath);
        }
        outputChannel.show();
    }
}

/** Ask which emit kind to show, then show it. */
export async function showEmitPick(
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel
): Promise<void> {
    const picked = await vscode.window.showQuickPick([...TEXT_EMIT_KINDS], {
        title: 'IRIS: Show Compiler Output',
        placeHolder: 'Select an --emit kind',
    });
    if (picked) {
        await showEmit(picked as EmitKind, getIrisExe, outputChannel);
    }
}

export function showFullVersion(
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel
): void {
    clearVersionCache();
    const info = getIrisVersionInfo(getIrisExe);
    if (info.fullOutput) {
        outputChannel.clear();
        outputChannel.appendLine('=== IRIS Compiler Version Info ===');
        outputChannel.appendLine('');
        outputChannel.appendLine(info.fullOutput);
        outputChannel.show(true);
    } else {
        vscode.window.showWarningMessage(
            'Could not retrieve IRIS version info. Is the iris executable in your PATH?'
        );
    }
}

/** Run an iris subcommand in a dedicated terminal, reusing one per name. */
function runInTerminal(name: string, exe: string, args: string[], cwd?: string): void {
    const existing = vscode.window.terminals.find(t => t.name === name);
    if (existing) {
        existing.dispose();
    }
    const terminal = vscode.window.createTerminal({ name, shellPath: exe, shellArgs: args, cwd });
    terminal.show();
}

/** The active .iris file, saved, or undefined with a warning already shown. */
async function activeIrisFile(): Promise<string | undefined> {
    const editor = vscode.window.activeTextEditor;
    if (!editor || !editor.document.fileName.endsWith('.iris')) {
        vscode.window.showWarningMessage('Open an .iris file first.');
        return undefined;
    }
    await editor.document.save();
    return editor.document.fileName;
}

/**
 * Explain an error code via `iris explain`.
 *
 * Defaults to a code taken from a diagnostic at the cursor, since that is
 * almost always the one being asked about.
 */
export async function explainErrorCode(
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel
): Promise<void> {
    const editor = vscode.window.activeTextEditor;
    let seed = '';
    if (editor) {
        const pos = editor.selection.active;
        const hit = vscode.languages
            .getDiagnostics(editor.document.uri)
            .find(d => d.range.contains(pos) && typeof d.code === 'string');
        if (hit) {
            seed = String(hit.code);
        }
    }

    const code = await vscode.window.showInputBox({
        title: 'IRIS: Explain Error Code',
        prompt: 'Error code to explain (e.g. E0100)',
        value: seed,
        validateInput: v => (/^E\d{4}$/i.test(v.trim()) ? null : 'Expected a code like E0100'),
    });
    if (!code) {
        return;
    }

    const exe = getIrisExe();
    try {
        const out = child_process.execFileSync(exe, ['explain', code.trim().toUpperCase()], {
            encoding: 'utf8',
            timeout: 10000,
            windowsHide: true,
        });
        outputChannel.clear();
        outputChannel.appendLine(out);
        outputChannel.show(true);
    } catch (err: any) {
        outputChannel.appendLine(`iris explain ${code} failed: ${err.message || err}`);
        outputChannel.show();
    }
}

/** Verify formatting without modifying, via `iris fmt --check`. */
export async function checkFormatting(
    getIrisExe: () => string,
    outputChannel: vscode.OutputChannel
): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    const exe = getIrisExe();
    try {
        const out = child_process.execFileSync(exe, ['fmt', '--check', filePath], {
            encoding: 'utf8',
            timeout: 30000,
            windowsHide: true,
        });
        outputChannel.appendLine(out.trim() || `✓ ${path.basename(filePath)} is correctly formatted.`);
        vscode.window.showInformationMessage(`IRIS: ${path.basename(filePath)} is correctly formatted.`);
    } catch (err: any) {
        // A non-zero exit is the documented signal for "needs formatting".
        const detail = (err?.stdout?.toString?.() ?? '') + (err?.stderr?.toString?.() ?? '');
        outputChannel.appendLine(detail.trim() || `${path.basename(filePath)} needs formatting.`);
        outputChannel.show(true);
        const choice = await vscode.window.showWarningMessage(
            `IRIS: ${path.basename(filePath)} is not correctly formatted.`,
            'Format Now',
        );
        if (choice === 'Format Now') {
            await vscode.commands.executeCommand('editor.action.formatDocument');
        }
    }
}

/** Run every `test_` function in the active file. */
export async function runFileTests(getIrisExe: () => string): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    runInTerminal('IRIS Tests', getIrisExe(), ['test', filePath], path.dirname(filePath));
}

/** Run all tests in the workspace (`iris test`). */
export function runWorkspaceTests(getIrisExe: () => string): void {
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    runInTerminal('IRIS Test Suite', getIrisExe(), ['test'], cwd);
}

/** Format all .iris files in the workspace (`iris fmt`). */
export function fmtWorkspace(getIrisExe: () => string): void {
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    runInTerminal('IRIS Formatter', getIrisExe(), ['fmt'], cwd);
}


/** Run benchmarks in the active file. */
export async function runBenchmarks(getIrisExe: () => string): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    runInTerminal('IRIS Bench', getIrisExe(), ['bench', filePath], path.dirname(filePath));
}

/** Generate HTML documentation from doc comments. */
export async function generateDocs(getIrisExe: () => string): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    runInTerminal('IRIS Docs', getIrisExe(), ['docs', filePath], path.dirname(filePath));
}

/** Drive the package manager: `iris pkg <cmd>`. */
export async function pkgCommand(getIrisExe: () => string): Promise<void> {
    const sub = await vscode.window.showQuickPick(
        ['init', 'add', 'remove', 'install', 'update', 'list', 'check', 'build', 'run', 'vendor', 'search', 'publish'],
        { title: 'IRIS: Package Manager', placeHolder: 'Select a pkg subcommand' },
    );
    if (!sub) {
        return;
    }

    const args = ['pkg', sub];
    if (sub === 'add' || sub === 'remove' || sub === 'search' || sub === 'update') {
        const promptMap: Record<string, string> = {
            add: 'Package name, --path <dir>, --git <url>, or --registry <name>',
            remove: 'Package name to remove',
            search: 'Search text for registry packages',
            update: 'Optional dependency name (leave empty to update all)',
        };
        const value = await vscode.window.showInputBox({
            title: `IRIS: pkg ${sub}`,
            prompt: promptMap[sub],
        });
        if (value === undefined) {
            return;
        }
        if (value.trim()) {
            args.push(...value.trim().split(/\s+/));
        } else if (sub !== 'update') {
            return;
        }
    }

    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    runInTerminal('IRIS Package Manager', getIrisExe(), args, cwd);
}

/** Package and optionally install a standalone desktop application (`iris bundle`). */
export async function bundleApp(getIrisExe: () => string): Promise<void> {
    const choice = await vscode.window.showQuickPick(
        [
            { label: 'Portable Directory (auto)', format: 'auto', install: false },
            { label: 'Portable Zip Archive (--format zip)', format: 'zip', install: false },
            { label: 'Windows Inno Setup Script (--format inno)', format: 'inno', install: false },
            { label: 'macOS App Bundle (--format app)', format: 'app', install: false },
            { label: 'Linux AppImage / AppDir (--format appimage)', format: 'appimage', install: false },
            { label: 'Bundle & Install onto Host OS (--install)', format: 'auto', install: true },
        ],
        { title: 'IRIS: Bundle Standalone Application', placeHolder: 'Choose distribution action' },
    );
    if (!choice) {
        return;
    }

    const activeFile = vscode.window.activeTextEditor?.document.fileName;
    const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    const targetDir = activeFile && activeFile.endsWith('.iris')
        ? path.dirname(activeFile)
        : workspaceRoot;
    if (!targetDir) {
        vscode.window.showWarningMessage('Open a workspace folder or .iris project file first.');
        return;
    }

    const manifestPath = path.join(targetDir, 'iris.toml');
    const bundleInput = fs.existsSync(manifestPath)
        ? targetDir
        : (activeFile && activeFile.endsWith('.iris') ? activeFile : targetDir);

    const args = ['bundle'];
    if (vscode.workspace.getConfiguration('iris').get<boolean>('bundle.release', true)) {
        args.push('--release');
    }
    if (choice.install) {
        args.push('--install');
    } else if (choice.format !== 'auto') {
        args.push('--format', choice.format);
    }
    args.push(bundleInput);

    runInTerminal('IRIS Bundle', getIrisExe(), args, targetDir);
}

/** Scaffold a new IRIS project (`iris new [TEMPLATE] NAME`). */
export async function newProjectFromTemplate(getIrisExe: () => string): Promise<void> {
    const template = await vscode.window.showQuickPick(
        [
            { label: 'ui', description: 'Native desktop UI application with std.ui and [app] manifest' },
            { label: 'app', description: 'General native application project' },
            { label: 'cli', description: 'Command-line tool parsing args()' },
            { label: 'server', description: 'HTTP / TCP network service' },
            { label: 'ros2', description: 'ROS 2 robotics node (std.ros2)' },
            { label: 'controller', description: 'Allocation-free controller loop' },
        ],
        { title: 'IRIS: New Project Template', placeHolder: 'Select project template' },
    );
    if (!template) {
        return;
    }
    const name = await vscode.window.showInputBox({
        title: `IRIS: New ${template.label} Project`,
        prompt: 'Enter project directory name',
        value: `my_${template.label}_project`,
        validateInput: v => (/^[a-zA-Z0-9_-]+$/.test(v.trim()) ? null : 'Use letters, digits, _ or -'),
    });
    if (!name) {
        return;
    }
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    runInTerminal('IRIS New Project', getIrisExe(), ['new', template.label, name.trim()], cwd);
}

/** Run `iris check` across the workspace or active directory without linking. */
export function checkProject(getIrisExe: () => string): void {
    const cfg = vscode.workspace.getConfiguration('iris');
    const args: string[] = [];
    if (cfg.get<boolean>('strictEffects', false)) {
        args.push('--strict-effects');
    }
    args.push('check');
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath
        ?? (vscode.window.activeTextEditor ? path.dirname(vscode.window.activeTextEditor.document.fileName) : undefined);
    runInTerminal('IRIS Check', getIrisExe(), args, cwd);
}

/** Watch the active .iris file (`iris watch [--test|--check] <file>`). */
export async function watchFile(getIrisExe: () => string): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    const mode = await vscode.window.showQuickPick(
        [
            { label: 'run', description: 'Re-run program on every save' },
            { label: 'test', description: 'Re-run test_* suite on every save (--test)' },
            { label: 'check', description: 'Re-check types, borrows, and effects on every save (--check)' },
        ],
        { title: 'IRIS: Watch File', placeHolder: 'Select watch mode' },
    );
    if (!mode) {
        return;
    }
    const args = ['watch'];
    if (mode.label === 'test') { args.push('--test'); }
    if (mode.label === 'check') { args.push('--check'); }
    args.push(filePath);
    runInTerminal('IRIS Watch', getIrisExe(), args, path.dirname(filePath));
}

/** Run `iris doctor` end-to-end toolchain diagnostics. */
export function runDoctor(getIrisExe: () => string): void {
    const cwd = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    runInTerminal('IRIS Doctor', getIrisExe(), ['doctor'], cwd);
}

/** Run profiler instrumentation (`iris profile <file>`). */
export async function profileFile(getIrisExe: () => string): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    runInTerminal('IRIS Profile', getIrisExe(), ['profile', filePath], path.dirname(filePath));
}

/** Build an allocation-free microcontroller bundle (`iris embedded --target <target> <file>`). */
export async function buildEmbedded(getIrisExe: () => string): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    const target = await vscode.window.showQuickPick(
        ['cortex-m4f', 'cortex-m33', 'esp32-c3', 'esp32', 'arduino-uno'],
        { title: 'IRIS: Embedded Target', placeHolder: 'Select microcontroller profile' },
    );
    if (!target) {
        return;
    }
    runInTerminal('IRIS Embedded', getIrisExe(), ['embedded', '--target', target, filePath], path.dirname(filePath));
}

/** Generate IRIS FFI bindings from a C/C++ header (`iris bindgen`). */
export async function runBindgen(getIrisExe: () => string): Promise<void> {
    const uris = await vscode.window.showOpenDialog({
        canSelectMany: false,
        title: 'Select C/C++ Header (.h/.hpp)',
        filters: { 'C/C++ Headers': ['h', 'hpp', 'hh'] },
    });
    if (!uris || uris.length === 0) {
        return;
    }
    const headerPath = uris[0].fsPath;
    const libName = await vscode.window.showInputBox({
        title: 'IRIS Bindgen',
        prompt: 'Library name for @link(lib = "...") (optional)',
    });
    const outPath = headerPath.replace(/\.(h|hpp|hh)$/i, '_ffi.iris');
    const args = ['bindgen'];
    if (/\.hpp$|\.hh$/i.test(headerPath)) {
        args.push('--cpp');
    }
    if (libName && libName.trim()) {
        args.push('-l', libName.trim());
    }
    args.push('-o', outPath, headerPath);
    runInTerminal('IRIS Bindgen', getIrisExe(), args, path.dirname(headerPath));
}

/** Collect local compiler bug-report folder (`iris bug-report <file>`). */
export async function collectBugReport(getIrisExe: () => string): Promise<void> {
    const filePath = await activeIrisFile();
    if (!filePath) {
        return;
    }
    runInTerminal('IRIS Bug Report', getIrisExe(), ['bug-report', '--run', filePath], path.dirname(filePath));
}

function parseAndShowErrors(stderr: string, filePath: string): void {
    const uri = vscode.Uri.file(filePath);
    const diags: vscode.Diagnostic[] = [];

    const richErrorPattern = /error(?:\[(\w+)\])?\s*:\s*(.+)/g;
    const locationPattern = /-->\s*[^:]+:(\d+):(\d+)/;

    let match: RegExpExecArray | null;
    richErrorPattern.lastIndex = 0;
    const lines = stderr.split('\n');

    for (let i = 0; i < lines.length; i++) {
        const line = lines[i];
        richErrorPattern.lastIndex = 0;
        match = richErrorPattern.exec(line);
        if (match) {
            const errorCode = match[1] || undefined;
            const message = match[2].trim();
            let lineNum = 0;
            let colNum = 0;
            if (i + 1 < lines.length) {
                const locMatch = lines[i + 1].match(locationPattern);
                if (locMatch) {
                    lineNum = Math.max(0, parseInt(locMatch[1]) - 1);
                    colNum = Math.max(0, parseInt(locMatch[2]) - 1);
                }
            }
            const range = new vscode.Range(lineNum, colNum, lineNum, colNum + 20);
            const diag = new vscode.Diagnostic(range, message, vscode.DiagnosticSeverity.Error);
            if (errorCode) {
                diag.code = errorCode;
            }
            diag.source = 'iris';
            diags.push(diag);
        }
    }

    if (diags.length === 0) {
        const simplePattern = /error:\s*(.+)/gi;
        const lineMatch = /line (\d+)/i;
        for (const line of lines) {
            simplePattern.lastIndex = 0;
            const m = simplePattern.exec(line);
            if (m) {
                const lm = line.match(lineMatch);
                const lineNum = lm ? parseInt(lm[1]) - 1 : 0;
                const range = new vscode.Range(lineNum, 0, lineNum, 999);
                const diag = new vscode.Diagnostic(range, m[1].trim(), vscode.DiagnosticSeverity.Error);
                diag.source = 'iris';
                diags.push(diag);
            }
        }
    }

    if (diags.length > 0) {
        runDiagCollection.set(uri, diags);
    } else {
        runDiagCollection.delete(uri);
    }
}

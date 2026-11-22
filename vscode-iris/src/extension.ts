import * as vscode from 'vscode';
import { initStatusBar, updateStatusBar, showServerMenu } from './statusBar';
import { startLspClient, restartLsp, stopLsp, client } from './lspClient';
import * as fs from 'fs';
import { getIrisExe, resetIrisExeCache, diagnoseCandidates, workspaceBuilds } from './irisExe';
import {
    runIrisFile,
    runNamedFunction,
    debugIrisFile,
    openRepl,
    showEmit,
    showEmitPick,
    showFullVersion,
    explainErrorCode,
    checkFormatting,
    runFileTests,
    runWorkspaceTests,
    fmtWorkspace,
    runBenchmarks,
    generateDocs,
    pkgCommand,
    bundleApp,
    newProjectFromTemplate,
    checkProject,
    watchFile,
    runDoctor,
    profileFile,
    buildEmbedded,
    runBindgen,
    collectBugReport,
    IrisEmitProvider,
    runDiagCollection,
} from './commands';
import { IrisDebugAdapterFactory, IrisDebugConfigurationProvider } from './debugAdapter';
import { IrisCodeLensProvider } from './codelens';
import { IrisTaskProvider } from './tasks';
import { IrisTestController } from './testController';

let outputChannel: vscode.OutputChannel;
let serverOutputChannel: vscode.OutputChannel;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    outputChannel = vscode.window.createOutputChannel('IRIS');
    serverOutputChannel = vscode.window.createOutputChannel('IRIS Language Server');
    context.subscriptions.push(outputChannel, serverOutputChannel);

    // Initialize Status Bar
    initStatusBar(context);
    updateStatusBar('starting', getIrisExe);

    // Register Commands
    context.subscriptions.push(
        vscode.commands.registerCommand('iris.runFile', () => runIrisFile(context, 'run', getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.buildFile', () => runIrisFile(context, 'build', getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.debugFile', (uri?: vscode.Uri | string) => debugIrisFile(uri, getIrisExe)),
        vscode.commands.registerCommand('iris.debugFileNative', (uri?: vscode.Uri | string) => debugIrisFile(uri, getIrisExe, true)),
        vscode.commands.registerCommand('iris.openRepl', () => openRepl(context, getIrisExe)),
        vscode.commands.registerCommand('iris.restartLsp', () => restartLsp(context, getIrisExe, serverOutputChannel)),
        vscode.commands.registerCommand('iris.stopLsp', () => stopLsp(getIrisExe)),
        vscode.commands.registerCommand('iris.showIR', () => showEmit('ir', getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.showLLVM', () => showEmit('llvm', getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.showVersion', () => showFullVersion(getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.showEmit', () => showEmitPick(getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.showGraph', () => showEmit('graph', getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.showCuda', () => showEmit('cuda', getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.showSimd', () => showEmit('simd', getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.explainError', () => explainErrorCode(getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.checkFormat', () => checkFormatting(getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.runTests', () => runFileTests(getIrisExe)),
        vscode.commands.registerCommand('iris.runWorkspaceTests', () => runWorkspaceTests(getIrisExe)),
        vscode.commands.registerCommand('iris.fmtWorkspace', () => fmtWorkspace(getIrisExe)),
        vscode.commands.registerCommand('iris.runBench', () => runBenchmarks(getIrisExe)),
        vscode.commands.registerCommand('iris.generateDocs', () => generateDocs(getIrisExe)),
        vscode.commands.registerCommand('iris.pkg', () => pkgCommand(getIrisExe)),
        vscode.commands.registerCommand('iris.buildGui', () => runIrisFile(context, 'build', getIrisExe, outputChannel, true)),
        vscode.commands.registerCommand('iris.bundleApp', () => bundleApp(getIrisExe)),
        vscode.commands.registerCommand('iris.newProject', () => newProjectFromTemplate(getIrisExe)),
        vscode.commands.registerCommand('iris.checkWorkspace', () => checkProject(getIrisExe)),
        vscode.commands.registerCommand('iris.watchFile', () => watchFile(getIrisExe)),
        vscode.commands.registerCommand('iris.doctor', () => runDoctor(getIrisExe)),
        vscode.commands.registerCommand('iris.profileFile', () => profileFile(getIrisExe)),
        vscode.commands.registerCommand('iris.buildEmbedded', () => buildEmbedded(getIrisExe)),
        vscode.commands.registerCommand('iris.bindgen', () => runBindgen(getIrisExe)),
        vscode.commands.registerCommand('iris.bugReport', () => collectBugReport(getIrisExe)),
        vscode.commands.registerCommand('iris.diagnostics', () => {
            outputChannel.clear();
            outputChannel.appendLine('=== IRIS executable resolution ===');
            outputChannel.appendLine('');
            outputChannel.appendLine(`selected: ${getIrisExe()}`);
            outputChannel.appendLine('');
            outputChannel.appendLine('candidates (best first):');
            for (const line of diagnoseCandidates()) {
                outputChannel.appendLine(line);
            }
            outputChannel.show(true);
        }),
        vscode.commands.registerCommand('iris.runFunction', (uri: string, fnName: string) =>
            runNamedFunction(uri, fnName, getIrisExe, outputChannel)),
        vscode.commands.registerCommand('iris.serverMenu', () => showServerMenu(context, client !== undefined, {
            restartLsp: () => restartLsp(context, getIrisExe, serverOutputChannel),
            stopLsp: () => stopLsp(getIrisExe),
            startLsp: () => startLspClient(context, getIrisExe, serverOutputChannel),
            showServerOutput: () => serverOutputChannel.show(),
            openRepl: () => openRepl(context, getIrisExe),
            showFullVersion: () => showFullVersion(getIrisExe, outputChannel),
        })),
        vscode.commands.registerCommand('iris.runTest', (uriStr: string, fnName: string) => {
            const filePath = vscode.Uri.parse(uriStr).fsPath;
            const terminal = vscode.window.createTerminal({
                name: `IRIS Test - ${fnName}`,
                shellPath: getIrisExe(),
                shellArgs: ['test', filePath, '--filter', fnName],
            });
            terminal.show();
        }),
        vscode.commands.registerCommand('iris.debugTest', (uriStr: string, fnName: string) => {
            const filePath = vscode.Uri.parse(uriStr).fsPath;
            vscode.debug.startDebugging(vscode.workspace.getWorkspaceFolder(vscode.Uri.parse(uriStr)), {
                type: 'iris',
                request: 'launch',
                name: `Debug Test ${fnName}`,
                program: filePath,
                entryFunction: fnName,
                stopOnEntry: true,
            });
        }),
    );

    // Register CodeLens Provider
    context.subscriptions.push(
        vscode.languages.registerCodeLensProvider(
            { scheme: 'file', language: 'iris' },
            new IrisCodeLensProvider(),
        ),
    );

    // Register Text Document Content Provider for emitting IR/LLVM
    context.subscriptions.push(
        vscode.workspace.registerTextDocumentContentProvider('iris-emit', new IrisEmitProvider()),
    );

    // Register Debug Adapter Descriptor Factory
    context.subscriptions.push(
        vscode.debug.registerDebugAdapterDescriptorFactory('iris', new IrisDebugAdapterFactory(getIrisExe)),
        vscode.debug.registerDebugConfigurationProvider('iris', new IrisDebugConfigurationProvider()),
    );

    // Register Task Provider
    context.subscriptions.push(
        vscode.tasks.registerTaskProvider('iris', new IrisTaskProvider(getIrisExe)),
        new IrisTestController(getIrisExe, outputChannel),
    );

    // Format on save (opt-in)
    context.subscriptions.push(
        vscode.workspace.onWillSaveTextDocument(e => {
            const cfg = vscode.workspace.getConfiguration('iris');
            if (cfg.get<boolean>('formatOnSave', false) && e.document.languageId === 'iris') {
                const editor = vscode.window.visibleTextEditors.find(
                    ed => ed.document.uri.toString() === e.document.uri.toString(),
                );
                const tabSize =
                    typeof editor?.options.tabSize === 'number'
                        ? editor.options.tabSize
                        : cfg.get<number>('format.indentSize', 4);
                const insertSpaces =
                    typeof editor?.options.insertSpaces === 'boolean'
                        ? editor.options.insertSpaces
                        : true;
                e.waitUntil(
                    vscode.commands
                        .executeCommand<vscode.TextEdit[]>(
                            'vscode.executeFormatDocumentProvider',
                            e.document.uri,
                            { tabSize, insertSpaces },
                        )
                        .then(edits => edits ?? []),
                );
            }
        }),
    );

    // Settings that are only read at server startup need a restart to apply:
    // the executable choice, and the inlay-hint flags sent as
    // initializationOptions.
    context.subscriptions.push(
        vscode.workspace.onDidChangeConfiguration(async e => {
            if (e.affectsConfiguration('iris.executablePath')) {
                resetIrisExeCache();
                await restartLsp(context, getIrisExe, serverOutputChannel);
            } else if (
                e.affectsConfiguration('iris.inlayHints.enabled') ||
                e.affectsConfiguration('iris.inlayHints.typeHints') ||
                e.affectsConfiguration('iris.format.indentSize') ||
                e.affectsConfiguration('iris.format.maxLineWidth') ||
                e.affectsConfiguration('iris.maxNumberOfProblems')
            ) {
                await restartLsp(context, getIrisExe, serverOutputChannel);
            }
        }),
    );

    // Start LSP Client
    await startLspClient(context, getIrisExe, serverOutputChannel);

    // Follow the compiler being built in the workspace: when `cargo build`
    // replaces target/{debug,release}/iris, restart the server on the new
    // build so diagnostics come from the compiler as it now is. Polled rather
    // than watched through VS Code, whose `files.watcherExclude` commonly
    // hides `target/`. An explicit `iris.executablePath` is left alone.
    let pending: NodeJS.Timeout | undefined;
    const onBuild = (curr: fs.Stats, prev: fs.Stats) => {
        const configured = vscode.workspace.getConfiguration('iris').get<string>('executablePath', '').trim();
        if (curr.mtimeMs === prev.mtimeMs || curr.mtimeMs === 0 || (configured && configured !== 'iris' && configured !== 'iris.exe')) {
            return;
        }
        if (pending) {
            clearTimeout(pending);
        }
        // Let the linker finish writing before the binary is probed and copied.
        pending = setTimeout(() => {
            pending = undefined;
            serverOutputChannel.appendLine('IRIS: the workspace compiler was rebuilt; restarting the language server');
            void restartLsp(context, getIrisExe, serverOutputChannel, true);
        }, 3000);
    };
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
        for (const build of workspaceBuilds(folder.uri.fsPath)) {
            fs.watchFile(build, { interval: 2000 }, onBuild);
            context.subscriptions.push({ dispose: () => fs.unwatchFile(build, onBuild) });
        }
    }
}

export function deactivate(): Thenable<void> | undefined {
    runDiagCollection.dispose();
    return client?.stop();
}

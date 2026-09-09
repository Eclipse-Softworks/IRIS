import * as vscode from 'vscode';
import * as childProcess from 'child_process';
import * as path from 'path';

const TEST_FUNCTION = /^(?:pub\s+)?(?:async\s+)?def\s+(test_[A-Za-z_][A-Za-z0-9_]*)\s*\(\s*\)/gm;

/** Replace comments and quoted literals with spaces while preserving offsets. */
function codeOnly(source: string): string {
    // split('') intentionally keeps UTF-16 code units so RegExp indices match
    // VS Code's TextDocument.positionAt offsets even before non-BMP text.
    const chars = source.split('');
    let i = 0;
    while (i < chars.length) {
        if (chars[i] === '/' && chars[i + 1] === '/') {
            while (i < chars.length && chars[i] !== '\n') {
                chars[i++] = ' ';
            }
        } else if (chars[i] === '/' && chars[i + 1] === '*') {
            let depth = 0;
            while (i < chars.length) {
                if (chars[i] === '/' && chars[i + 1] === '*') {
                    chars[i++] = ' ';
                    chars[i++] = ' ';
                    depth++;
                } else if (chars[i] === '*' && chars[i + 1] === '/') {
                    chars[i++] = ' ';
                    chars[i++] = ' ';
                    depth--;
                    if (depth === 0) { break; }
                } else {
                    if (chars[i] !== '\n') { chars[i] = ' '; }
                    i++;
                }
            }
        } else if (chars[i] === '"' || chars[i] === "'") {
            const quote = chars[i];
            chars[i++] = ' ';
            while (i < chars.length) {
                if (chars[i] === '\\') {
                    chars[i++] = ' ';
                    if (i < chars.length && chars[i] !== '\n') { chars[i] = ' '; }
                    i++;
                } else if (chars[i] === quote) {
                    chars[i++] = ' ';
                    break;
                } else {
                    if (chars[i] !== '\n') { chars[i] = ' '; }
                    i++;
                }
            }
        } else {
            i++;
        }
    }
    return chars.join('');
}

/** Native VS Code Test Explorer integration for zero-argument `test_*` functions. */
export class IrisTestController implements vscode.Disposable {
    private readonly controller = vscode.tests.createTestController('irisTests', 'IRIS Tests');
    private readonly subscriptions: vscode.Disposable[] = [];

    constructor(
        private readonly getIrisExe: () => string,
        private readonly output: vscode.OutputChannel,
    ) {
        this.controller.resolveHandler = async item => {
            if (item?.uri) {
                await this.discoverUri(item.uri);
            } else {
                await this.discoverWorkspace();
            }
        };
        this.controller.refreshHandler = () => this.discoverWorkspace();

        this.subscriptions.push(
            this.controller,
            this.controller.createRunProfile(
                'Run',
                vscode.TestRunProfileKind.Run,
                (request, token) => this.run(request, token),
                true,
            ),
            this.controller.createRunProfile(
                'Debug',
                vscode.TestRunProfileKind.Debug,
                (request, token) => this.debug(request, token),
                true,
            ),
        );

        const watcher = vscode.workspace.createFileSystemWatcher('**/*.iris');
        this.subscriptions.push(
            watcher,
            watcher.onDidCreate(uri => this.discoverUri(uri)),
            watcher.onDidChange(uri => this.discoverUri(uri)),
            watcher.onDidDelete(uri => this.controller.items.delete(this.fileId(uri))),
            vscode.workspace.onDidSaveTextDocument(document => {
                if (document.languageId === 'iris') {
                    void this.discoverDocument(document);
                }
            }),
        );

        void this.discoverWorkspace();
    }

    dispose(): void {
        for (const subscription of this.subscriptions) {
            subscription.dispose();
        }
    }

    private fileId(uri: vscode.Uri): string {
        return `file:${uri.toString()}`;
    }

    private testId(uri: vscode.Uri, name: string): string {
        return `${this.fileId(uri)}::${name}`;
    }

    private async discoverWorkspace(): Promise<void> {
        const uris = await vscode.workspace.findFiles('**/*.iris', '**/{target,node_modules,iris_packages}/**');
        await Promise.all(uris.map(uri => this.discoverUri(uri)));
    }

    private async discoverUri(uri: vscode.Uri): Promise<void> {
        try {
            const document = await vscode.workspace.openTextDocument(uri);
            await this.discoverDocument(document);
        } catch (error) {
            this.output.appendLine(`IRIS test discovery failed for ${uri.fsPath}: ${String(error)}`);
        }
    }

    private async discoverDocument(document: vscode.TextDocument): Promise<void> {
        const uri = document.uri;
        const fileId = this.fileId(uri);
        const found: Array<{ name: string; range: vscode.Range }> = [];
        TEST_FUNCTION.lastIndex = 0;
        const source = document.getText();
        const searchable = codeOnly(source);
        let match: RegExpExecArray | null;
        while ((match = TEST_FUNCTION.exec(searchable)) !== null) {
            const start = document.positionAt(match.index);
            const end = document.positionAt(match.index + match[0].length);
            found.push({ name: match[1], range: new vscode.Range(start, end) });
        }

        if (found.length === 0) {
            this.controller.items.delete(fileId);
            return;
        }

        let fileItem = this.controller.items.get(fileId);
        if (!fileItem) {
            fileItem = this.controller.createTestItem(fileId, path.basename(uri.fsPath), uri);
            fileItem.canResolveChildren = false;
            this.controller.items.add(fileItem);
        }

        const liveIds = new Set<string>();
        for (const test of found) {
            const id = this.testId(uri, test.name);
            liveIds.add(id);
            let item = fileItem.children.get(id);
            if (!item) {
                item = this.controller.createTestItem(id, test.name, uri);
                fileItem.children.add(item);
            }
            item.range = test.range;
        }
        for (const [id] of fileItem.children) {
            if (!liveIds.has(id)) {
                fileItem.children.delete(id);
            }
        }
    }

    private selectedTests(request: vscode.TestRunRequest): vscode.TestItem[] {
        const excluded = new Set(request.exclude?.map(item => item.id) ?? []);
        const selected: vscode.TestItem[] = [];
        const visit = (item: vscode.TestItem): void => {
            if (excluded.has(item.id)) {
                return;
            }
            if (item.id.includes('::')) {
                selected.push(item);
            } else {
                item.children.forEach(visit);
            }
        };
        if (request.include?.length) {
            request.include.forEach(visit);
        } else {
            this.controller.items.forEach(visit);
        }
        return selected;
    }

    private async run(request: vscode.TestRunRequest, token: vscode.CancellationToken): Promise<void> {
        const run = this.controller.createTestRun(request);
        for (const item of this.selectedTests(request)) {
            if (token.isCancellationRequested || !item.uri) {
                run.skipped(item);
                continue;
            }
            run.started(item);
            const started = Date.now();
            const result = await this.runProcess(
                ['test', item.uri.fsPath, '--filter', item.label, '--no-color'],
                path.dirname(item.uri.fsPath),
                token,
            );
            run.appendOutput(result.output.replace(/\r?\n/g, '\r\n'), undefined, item);
            if (result.cancelled) {
                run.skipped(item);
            } else if (result.code === 0) {
                run.passed(item, Date.now() - started);
            } else {
                run.failed(item, new vscode.TestMessage(result.output || `iris test exited ${result.code}`), Date.now() - started);
            }
        }
        run.end();
    }

    private async debug(request: vscode.TestRunRequest, token: vscode.CancellationToken): Promise<void> {
        const run = this.controller.createTestRun(request);
        for (const item of this.selectedTests(request)) {
            if (token.isCancellationRequested || !item.uri) {
                run.skipped(item);
                continue;
            }
            run.started(item);
            const started = await vscode.debug.startDebugging(vscode.workspace.getWorkspaceFolder(item.uri), {
                type: 'iris',
                request: 'launch',
                name: `Debug ${item.label}`,
                program: item.uri.fsPath,
                entryFunction: item.label,
                stopOnEntry: true,
            });
            if (started) {
                run.passed(item);
            } else {
                run.errored(item, new vscode.TestMessage('The IRIS debug adapter did not start.'));
            }
        }
        run.end();
    }

    private runProcess(
        args: string[],
        cwd: string,
        token: vscode.CancellationToken,
    ): Promise<{ code: number | null; output: string; cancelled: boolean }> {
        return new Promise(resolve => {
            const proc = childProcess.spawn(this.getIrisExe(), args, { cwd, shell: false, windowsHide: true });
            let output = '';
            proc.stdout.on('data', data => { output += data.toString(); });
            proc.stderr.on('data', data => { output += data.toString(); });
            const cancellation = token.onCancellationRequested(() => proc.kill());
            proc.on('error', error => {
                cancellation.dispose();
                resolve({ code: -1, output: `${output}${error.message}\n`, cancelled: token.isCancellationRequested });
            });
            proc.on('close', code => {
                cancellation.dispose();
                resolve({ code, output, cancelled: token.isCancellationRequested });
            });
        });
    }
}

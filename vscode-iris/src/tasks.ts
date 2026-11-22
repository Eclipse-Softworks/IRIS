import * as vscode from 'vscode';

/** Each provided task: its `iris` arguments and task group. */
const TASKS: Record<string, { args: string[]; group?: vscode.TaskGroup }> = {
    // No group: VS Code's TaskGroup has only Clean, Build, Rebuild and Test,
    // so run/diagnostic tasks stay ungrouped.
    run: { args: ['run', '${file}'] },
    build: { args: ['build', '${file}'], group: vscode.TaskGroup.Build },
    // Native binary with DWARF debug info, for gdb/lldb or a preLaunchTask.
    'build-debug': { args: ['-g', 'build', '${file}'], group: vscode.TaskGroup.Build },
    'build-gui': { args: ['build', '--gui', '${file}'], group: vscode.TaskGroup.Build },
    bundle: { args: ['bundle', '--release'], group: vscode.TaskGroup.Build },
    check: { args: ['check'], group: vscode.TaskGroup.Test },
    test: { args: ['test'], group: vscode.TaskGroup.Test },
    bench: { args: ['bench', '${file}'] },
    profile: { args: ['profile', '${file}'] },
    doctor: { args: ['doctor'] },
};

export class IrisTaskProvider implements vscode.TaskProvider {
    private readonly getIrisExe: () => string;

    constructor(getIrisExe: () => string) {
        this.getIrisExe = getIrisExe;
    }

    public provideTasks(): vscode.ProviderResult<vscode.Task[]> {
        return Object.keys(TASKS).map(name =>
            this.makeTask({ type: 'iris', task: name }, vscode.TaskScope.Workspace, name)!,
        );
    }

    public resolveTask(task: vscode.Task): vscode.Task | undefined {
        return this.makeTask(
            task.definition,
            task.scope ?? vscode.TaskScope.Workspace,
            task.name || task.definition.task,
        );
    }

    private makeTask(
        definition: vscode.TaskDefinition,
        scope: vscode.TaskScope | vscode.WorkspaceFolder,
        name: string,
    ): vscode.Task | undefined {
        const spec = TASKS[definition.task as string];
        if (!spec) {
            return undefined;
        }
        const task = new vscode.Task(
            definition,
            scope,
            name,
            'iris',
            new vscode.ProcessExecution(this.getIrisExe(), spec.args),
            '$iris',
        );
        if (spec.group) {
            task.group = spec.group;
        }
        return task;
    }
}

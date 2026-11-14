import * as vscode from 'vscode';

export class IrisDebugAdapterFactory implements vscode.DebugAdapterDescriptorFactory {
    private readonly getIrisExe: () => string;

    constructor(getIrisExe: () => string) {
        this.getIrisExe = getIrisExe;
    }

    createDebugAdapterDescriptor(): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
        // `iris dap` relays native sessions to the adapter named by
        // IRIS_NATIVE_DAP; forward the setting when one is configured.
        const nativeAdapter = vscode.workspace
            .getConfiguration('iris')
            .get<string>('debug.nativeAdapter', '')
            .trim();
        const options: vscode.DebugAdapterExecutableOptions = nativeAdapter
            ? { env: { ...process.env, IRIS_NATIVE_DAP: nativeAdapter } as { [key: string]: string } }
            : {};
        return new vscode.DebugAdapterExecutable(this.getIrisExe(), ['dap'], options);
    }
}

/**
 * Lets F5 work without a launch.json: an empty configuration debugs the
 * active .iris file, honouring the `iris.debug.*` settings.
 */
export class IrisDebugConfigurationProvider implements vscode.DebugConfigurationProvider {
    provideDebugConfigurations(): vscode.ProviderResult<vscode.DebugConfiguration[]> {
        return [
            {
                type: 'iris',
                request: 'launch',
                name: 'Debug IRIS File',
                program: '${file}',
                stopOnEntry: false,
            },
            {
                type: 'iris',
                request: 'launch',
                name: 'Debug IRIS File (native)',
                program: '${file}',
                native: true,
            },
            {
                type: 'iris',
                request: 'attach',
                name: 'Attach to IRIS Native Process',
                native: true,
                target: 'localhost:3333',
            },
        ];
    }

    resolveDebugConfiguration(
        _folder: vscode.WorkspaceFolder | undefined,
        config: vscode.DebugConfiguration,
    ): vscode.ProviderResult<vscode.DebugConfiguration> {
        const settings = vscode.workspace.getConfiguration('iris');
        if (!config.type && !config.request && !config.name) {
            const editor = vscode.window.activeTextEditor;
            if (editor?.document.languageId !== 'iris') {
                // Not an IRIS file: let VS Code offer to create a launch.json.
                return config;
            }
            config.type = 'iris';
            config.request = 'launch';
            config.name = 'Debug IRIS File';
            config.program = '${file}';
            config.stopOnEntry = settings.get<boolean>('debug.stopOnEntry', false);
            config.native = settings.get<boolean>('debug.native', false);
        }
        if (config.request === 'attach') {
            if (
                config.processId === undefined &&
                config.pid === undefined &&
                config.target === undefined
            ) {
                return vscode.window
                    .showErrorMessage(
                        'IRIS native attach configuration requires "processId", "pid", or "target".',
                    )
                    .then(() => undefined);
            }
            if (config.native === undefined) {
                config.native = true;
            }
            return config;
        }
        if (!config.program) {
            return vscode.window
                .showErrorMessage('IRIS debug configuration has no "program" (.iris file) to debug.')
                .then(() => undefined);
        }
        return config;
    }
}

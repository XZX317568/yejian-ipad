import 'package:material_ui/material_ui.dart';
import 'package:flutter/services.dart';

import 'package:mangayomi/core/app_identity.dart';
import 'package:mangayomi/core/startup_failure.dart';

/// Shown when the reader cannot open its engine or library.
///
/// It intentionally offers only non-destructive actions. A "reset" button
/// would turn one recoverable permission error into a lost library.
class StartupRecoveryScreen extends StatelessWidget {
  const StartupRecoveryScreen({
    super.key,
    required this.failure,
    required this.dataPath,
    required this.onRetry,
    required this.retrying,
  });

  final StartupFailure failure;
  final String dataPath;
  final VoidCallback onRetry;
  final bool retrying;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final report = failure.asReport(
      dataPath: dataPath,
      platform: Theme.of(context).platform.name,
    );
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(24),
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: 620),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(
                    Icons.menu_book_rounded,
                    size: 56,
                    color: theme.colorScheme.primary,
                  ),
                  const SizedBox(height: 16),
                  Text(
                    '${AppIdentity.productName}没有打开',
                    textAlign: TextAlign.center,
                    style: theme.textTheme.headlineSmall,
                  ),
                  const SizedBox(height: 12),
                  Text(
                    failure.summary,
                    textAlign: TextAlign.center,
                    style: theme.textTheme.bodyMedium,
                  ),
                  if (failure.hint case final hint?) ...[
                    const SizedBox(height: 12),
                    Text(
                      hint,
                      textAlign: TextAlign.center,
                      style: theme.textTheme.bodySmall?.copyWith(
                        color: theme.colorScheme.onSurfaceVariant,
                      ),
                    ),
                  ],
                  const SizedBox(height: 24),
                  FilledButton(
                    onPressed: retrying ? null : onRetry,
                    child: Text(retrying ? '正在重试…' : '重试'),
                  ),
                  const SizedBox(height: 12),
                  SelectableText(
                    '书库数据位置\n$dataPath',
                    style: theme.textTheme.bodySmall?.copyWith(
                      fontFamily: 'monospace',
                    ),
                  ),
                  const SizedBox(height: 8),
                  OutlinedButton.icon(
                    icon: const Icon(Icons.content_copy_rounded),
                    label: const Text('复制书库位置'),
                    onPressed: () async {
                      await Clipboard.setData(ClipboardData(text: dataPath));
                      if (!context.mounted) return;
                      ScaffoldMessenger.of(
                        context,
                      ).showSnackBar(const SnackBar(content: Text('书库位置已复制。')));
                    },
                  ),
                  const SizedBox(height: 8),
                  OutlinedButton.icon(
                    icon: const Icon(Icons.copy_all_rounded),
                    label: const Text('复制错误报告'),
                    onPressed: () async {
                      await Clipboard.setData(ClipboardData(text: report));
                      if (!context.mounted) return;
                      ScaffoldMessenger.of(context).showSnackBar(
                        const SnackBar(content: Text('错误报告已复制，可以直接粘贴给维护者。')),
                      );
                    },
                  ),
                  const SizedBox(height: 16),
                  ExpansionTile(
                    tilePadding: EdgeInsets.zero,
                    title: const Text('技术细节'),
                    children: [
                      Align(
                        alignment: AlignmentDirectional.centerStart,
                        child: SelectableText(
                          failure.technical,
                          style: theme.textTheme.bodySmall?.copyWith(
                            fontFamily: 'monospace',
                          ),
                        ),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

import 'package:material_ui/material_ui.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mangayomi/core/startup_failure.dart';
import 'package:mangayomi/modules/startup/startup_recovery_screen.dart';

void main() {
  testWidgets('shows a readable recovery route without destructive actions', (
    tester,
  ) async {
    var retries = 0;
    await tester.pumpWidget(
      MaterialApp(
        theme: ThemeData(useMaterial3: true),
        home: StartupRecoveryScreen(
          failure: StartupFailure(
            stage: StartupStage.rust,
            error: StateError('missing rust library'),
            stack: StackTrace.current,
          ),
          dataPath: r'C:\Users\reader\Documents\Yejian',
          onRetry: () => retries += 1,
          retrying: false,
        ),
      ),
    );

    expect(find.text('页间没有打开'), findsOneWidget);
    expect(find.text('重试'), findsOneWidget);
    expect(find.textContaining('保留原数据文件'), findsOneWidget);
    expect(find.textContaining('Rust'), findsNothing);
    expect(find.textContaining(r'Documents\Yejian'), findsOneWidget);
    expect(find.text('复制错误报告'), findsOneWidget);
    expect(find.text('复制书库位置'), findsOneWidget);
    expect(find.text('重置'), findsNothing);
    expect(find.text('清除数据'), findsNothing);

    await tester.tap(find.text('重试'));
    expect(retries, 1);
  });

  testWidgets('disables retry while an attempt is running', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: StartupRecoveryScreen(
          failure: StartupFailure(
            stage: StartupStage.database,
            error: StateError('permission denied'),
            stack: StackTrace.current,
          ),
          dataPath: '/data/Yejian',
          onRetry: () {},
          retrying: true,
        ),
      ),
    );

    expect(find.text('正在重试…'), findsOneWidget);
    final button = tester.widget<FilledButton>(find.byType(FilledButton));
    expect(button.onPressed, isNull);
    expect(find.textContaining('写入权限'), findsOneWidget);
  });
}

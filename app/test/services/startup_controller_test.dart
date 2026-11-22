import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:mangayomi/core/startup_failure.dart';
import 'package:mangayomi/services/startup_controller.dart';

void main() {
  test(
    'a successful launch becomes ready and reports its data location',
    () async {
      final controller = StartupController(
        initialize: (controller) async {
          controller.setDataPath(r'C:\Users\reader\Documents\Yejian');
        },
      );

      await controller.start();

      expect(controller.ready, isTrue);
      expect(controller.failure, isNull);
      expect(controller.dataPath, endsWith(r'Documents\Yejian'));
    },
  );

  test('a failed launch exposes the stage and retry starts again', () async {
    var attempts = 0;
    final controller = StartupController(
      initialize: (controller) async {
        attempts += 1;
        if (attempts == 1) {
          controller.fail(
            StartupStage.rust,
            StateError('missing native library'),
            StackTrace.current,
          );
        }
        controller.setDataPath('/data/Yejian');
      },
    );

    await controller.start();
    expect(controller.ready, isFalse);
    expect(controller.failure?.stage, StartupStage.rust);
    expect(controller.failure?.summary, contains('保留原数据文件'));
    expect(controller.failure?.summary, isNot(contains('Rust')));

    await controller.start();
    expect(attempts, 2);
    expect(controller.ready, isTrue);
    expect(controller.failure, isNull);
  });

  test('a zone error before readiness becomes a visible failure', () async {
    final controller = StartupController(initialize: (_) async {});
    controller.reportUnhandled(StateError('bridge failed'), StackTrace.current);

    expect(controller.running, isFalse);
    expect(controller.failure, isNotNull);
    expect(controller.failure?.stage, StartupStage.rust);
  });

  test(
    'a zone error after readiness does not replace the running app',
    () async {
      final controller = StartupController(initialize: (_) async {});
      await controller.start();
      controller.reportUnhandled(
        StateError('later image error'),
        StackTrace.current,
      );

      expect(controller.ready, isTrue);
      expect(controller.failure, isNull);
    },
  );

  test('a zone error during initialize keeps the recovery screen', () async {
    final controller = StartupController(initialize: (_) async {});
    final attempt = controller.start();
    controller.reportUnhandled(
      StateError('bridge failed during launch'),
      StackTrace.current,
    );
    await attempt;

    expect(controller.ready, isFalse);
    expect(controller.failure, isNotNull);
    expect(controller.failure?.stage, StartupStage.rust);
  });

  test('a zone error cannot start a concurrent retry', () async {
    final gate = Completer<void>();
    var attempts = 0;
    final controller = StartupController(
      initialize: (_) async {
        attempts += 1;
        await gate.future;
      },
    );

    final first = controller.start();
    await Future<void>.delayed(Duration.zero);
    controller.reportUnhandled(
      StateError('bridge failed during launch'),
      StackTrace.current,
    );

    expect(controller.running, isTrue);
    expect(controller.failure, isNotNull);

    // The retry button must stay disabled while the first attempt is alive.
    await controller.start();
    expect(attempts, 1);

    gate.complete();
    await first;
    expect(controller.running, isFalse);
    expect(controller.ready, isFalse);
  });
}

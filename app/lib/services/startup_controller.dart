import 'package:flutter/foundation.dart';

import 'package:mangayomi/core/startup_failure.dart';

/// Owns the state of the one launch attempt that has to succeed before the
/// library can be shown.
///
/// The controller never deletes a directory or a database row. A failed attempt
/// can be retried against the same files.
class StartupController extends ChangeNotifier {
  StartupController({required this.initialize});

  final Future<void> Function(StartupController controller) initialize;

  bool _running = false;
  bool _ready = false;
  StartupFailure? _failure;
  String? _dataPath;

  bool get running => _running;
  bool get ready => _ready;
  StartupFailure? get failure => _failure;
  String? get dataPath => _dataPath;

  /// The first frame is drawn first, then this starts the native steps.
  Future<void> start() async {
    if (_running) return;
    _running = true;
    _failure = null;
    notifyListeners();
    try {
      await initialize(this);
      // An error reported from another zone while initialization was running
      // already produced a visible failure; do not replace it with the app.
      if (_failure == null) _ready = true;
    } catch (error, stack) {
      _failure ??= StartupFailure(
        stage: StartupStage.database,
        error: error,
        stack: stack,
      );
    } finally {
      _running = false;
      notifyListeners();
    }
  }

  /// Records the stage that failed and lets it stop initialization.
  Never fail(StartupStage stage, Object error, StackTrace stack) {
    _failure = StartupFailure(stage: stage, error: error, stack: stack);
    Error.throwWithStackTrace(error, stack);
  }

  void setDataPath(String path) {
    if (_dataPath == path) return;
    _dataPath = path;
    notifyListeners();
  }

  /// Failures from outside the awaited launch sequence are shown only while
  /// launch is unfinished. Once the reader is open, its normal error paths own
  /// the failure.
  void reportUnhandled(Object error, StackTrace stack) {
    if (_ready) return;
    _failure = StartupFailure(
      stage: StartupStage.rust,
      error: error,
      stack: stack,
      detail: '启动流程中出现未处理的异常。',
    );
    // Leave _running alone: initialize() is still on its way out. Clearing it
    // here would let the retry button start a second initialization against
    // the same native bridge and database while the first one is still running.
    notifyListeners();
  }
}

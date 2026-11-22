import 'package:flutter/foundation.dart';

/// The steps that must finish before the library can open.
enum StartupStage {
  rust('漫画引擎'),
  database('本地书库');

  const StartupStage(this.label);

  final String label;
}

/// A launch failure with enough context to show a useful recovery screen.
@immutable
class StartupFailure {
  const StartupFailure({
    required this.stage,
    required this.error,
    required this.stack,
    this.detail,
  });

  final StartupStage stage;
  final Object error;
  final StackTrace stack;
  final String? detail;

  /// The failed step in plain words, with only safe next actions.
  String get summary => switch (stage) {
    StartupStage.rust => '启动在“漫画引擎”这一步没有完成。可以重试；如果仍然失败，请保留原数据文件，并复制下面的错误报告。',
    StartupStage.database =>
      '启动在“本地书库”这一步没有完成。可以重试；如果仍然失败，请保留原数据文件，并复制下面的错误报告。',
  };

  /// A concrete action that is safe for this kind of failure.
  String? get hint {
    final text = '${error.toString()} ${detail ?? ''}'.toLowerCase();
    if (stage == StartupStage.database) {
      if (text.contains('mdbxerror (13)') ||
          text.contains('permission denied')) {
        return '看起来是数据目录没有写入权限。请检查下面的位置是否属于当前用户且可写，然后重试。';
      }
      if (text.contains('mdbxerror (15)') ||
          text.contains('block device required')) {
        return '数据目录所在的磁盘不允许数据库直接访问，请把它放回本机磁盘后重试。';
      }
      if (text.contains('password') || text.contains('encrypted')) {
        return '数据库似乎需要额外凭据。请保留原文件并复制下面的错误用于排查。';
      }
      return '可以先重试；如果仍然失败，请复制本页错误或数据位置交给维护者，原文件不要删除。';
    }
    return '可以先重试。如果仍然失败，请打开或复制数据位置查看文件是否完整，再交给维护者排查。';
  }

  /// A short technical line for the report, with the stage in front. Uses the
  /// plain stage label so an expanded details panel does not show engine names.
  String get technical => '${stage.label}: $error';

  String asReport({String? dataPath, String? platform}) => [
    '页间 (Yejian) 启动失败',
    '阶段: ${stage.label}',
    if (platform != null) '平台: $platform',
    if (dataPath != null) '书库数据位置: $dataPath',
    '',
    error.toString(),
    if (detail != null && detail!.trim().isNotEmpty) ...['', '细节: $detail'],
    '',
    stack.toString(),
  ].join('\n');
}

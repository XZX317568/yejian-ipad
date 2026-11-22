import 'package:flutter_test/flutter_test.dart';
import 'package:mangayomi/services/local_import_batch.dart';

void main() {
  test(
    'one failed item retains successes before and after it, in order',
    () async {
      final written = <String>[];
      final badFile = FormatException('truncated archive');
      var active = 0;
      final result = await importLocalFiles(
        ['before.cbz', 'broken.cbz', 'after.cbz'],
        pathOf: (file) => file,
        importFile: (file) async {
          expect(++active, 1);
          try {
            await Future<void>.delayed(Duration.zero);
            if (file == 'broken.cbz') throw badFile;
            written.add(file);
          } finally {
            active--;
          }
        },
      );
      expect(written, ['before.cbz', 'after.cbz']);
      expect(result.succeeded, written);
      expect(result.failures.single.path, 'broken.cbz');
      expect(result.failures.single.error, same(badFile));
    },
  );

  test('an all-failed batch is reported as zero successes', () async {
    final result = await importLocalFiles(
      ['broken.cbz'],
      pathOf: (file) => file,
      importFile: (_) async => throw StateError('unreadable'),
    );
    expect(result.succeeded, isEmpty);
    expect(result.failures.length, 1);
  });
}

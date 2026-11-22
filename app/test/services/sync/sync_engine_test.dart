import 'package:flutter_test/flutter_test.dart';
import 'package:mangayomi_sync/mangayomi_sync.dart';
import 'package:mangayomi_sync/testing.dart';

void main() {
  syncStoreSuite(
    test: test,
    setUp: setUp,
    tearDown: tearDown,
    openStore: MemorySyncStore.new,
  );

  test('fitting does not silently drop category memberships', () {
    final row = {
      'clientId': 1,
      'itemType': 'MANGA',
      'updatedAt': 1,
      'categoryClientIds': List.generate(201, (i) => i + 1),
    };
    final fitted = fitSyncRow('manga', row);
    expect(fitted.row, isNull);
    expect(fitted.tooLong, 'categoryClientIds');
    expect((row['categoryClientIds'] as List).length, 201);
  });

  test('display clipping leaves unicode characters and local data intact', () {
    final original = '${'x' * 9999}😀extra';
    final row = {
      'clientId': 1,
      'itemType': 'MANGA',
      'updatedAt': 1,
      'description': original,
    };
    final fitted = fitSyncRow('manga', row);
    expect(fitted.row!['description'], 'x' * 9999);
    expect(row['description'], original);
    validateSyncRequest({
      'manga': [fitted.row],
    });
  });
}

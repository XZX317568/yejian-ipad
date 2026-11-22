import 'package:isar_community/isar.dart';
part 'changed.g.dart';

@collection
@Name("ChangedPart")
class ChangedPart {
  Id? id;
  @enumerated
  late ActionType actionType;
  int? isarId;
  int? clientId;
  String data;
  int clientDate;

  ChangedPart({
    this.id = Isar.autoIncrement,
    required this.actionType,
    this.isarId,
    this.clientId,
    required this.data,
    required this.clientDate,
  });

  Map<String, dynamic> toJson() => {
    'action': actionType.name,
    'isarId': isarId,
    'clientId': clientId,
    'data': data,
    'clientDate': clientDate,
  };
}

/// One pending change exactly as it was captured for an upload.
///
/// Acknowledging an upload must not remove a newer change to the same local
/// row. The repository therefore deletes a stored row only while every field
/// still matches the captured version.
class ChangedPartVersion {
  const ChangedPartVersion({
    required this.id,
    required this.actionType,
    required this.isarId,
    required this.clientId,
    required this.data,
    required this.clientDate,
  });

  factory ChangedPartVersion.of(ChangedPart part) {
    final id = part.id;
    if (id == null) {
      throw ArgumentError('A changed part must be stored before it is sent');
    }
    return ChangedPartVersion(
      id: id,
      actionType: part.actionType,
      isarId: part.isarId,
      clientId: part.clientId,
      data: part.data,
      clientDate: part.clientDate,
    );
  }

  final int id;
  final ActionType actionType;
  final int? isarId;
  final int? clientId;
  final String data;
  final int clientDate;

  bool matches(ChangedPart part) =>
      part.id == id &&
      part.actionType == actionType &&
      part.isarId == isarId &&
      part.clientId == clientId &&
      part.data == data &&
      part.clientDate == clientDate;
}

enum ActionType {
  removeItem,
  removeCategory,
  removeChapter,
  removeHistory,
  removeUpdate,
  removeExtension,
  removeTrack,
}

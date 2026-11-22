// Adapted from mikufoxxx/Wenku8_Flutter v0.5.6, 2026-11-17.
// Original copyright and GPL notices: packages/shiori/third_party/wenku8/.
// Changes: Shiori theme/data integration; see WENKU8_ADAPTATION.md.
part of '../physical_reading_shelf.dart';

class _ArchiveSnapPhysics extends ScrollPhysics {
  const _ArchiveSnapPhysics({
    required this.pose,
    super.parent = const ClampingScrollPhysics(),
  });
  final _WavePose pose;
  @override
  _ArchiveSnapPhysics applyTo(ScrollPhysics? ancestor) =>
      _ArchiveSnapPhysics(pose: pose, parent: buildParent(ancestor));
  @override
  double adjustPositionForNewDimensions({
    required ScrollMetrics oldPosition,
    required ScrollMetrics newPosition,
    required bool isScrolling,
    required double velocity,
  }) {
    // Preserve fractional focus during layout instead of jumping a frame later.
    if (!oldPosition.hasViewportDimension ||
        oldPosition.viewportDimension <= 0) {
      return newPosition.pixels.clamp(
        newPosition.minScrollExtent,
        newPosition.maxScrollExtent,
      );
    }
    // Infinite extents keep extentBefore/After unchanged, so the framework's
    // last metrics can hold old pixels. Use live pixels with the old pitch.
    final focus =
        newPosition.pixels / _WavePose.pitchFor(oldPosition.viewportDimension);
    if (!isScrolling &&
        oldPosition.viewportDimension != newPosition.viewportDimension) {
      pose.holdLayoutFocus = true;
    }
    return (focus * _WavePose.pitchFor(newPosition.viewportDimension)).clamp(
      newPosition.minScrollExtent,
      newPosition.maxScrollExtent,
    );
  }

  @override
  Simulation? createBallisticSimulation(
    ScrollMetrics position,
    double velocity,
  ) {
    if (pose.holdLayoutFocus && velocity == 0) {
      pose.holdLayoutFocus = false;
      return null;
    }
    pose.holdLayoutFocus = false;
    if (position.outOfRange) {
      return super.createBallisticSimulation(position, velocity);
    }
    final pitch = _WavePose.pitchFor(position.viewportDimension);
    final simulation = ArchiveScrollSimulation(
      position: position.pixels,
      velocity: velocity,
      pitch: pitch,
      minimumVelocity: minFlingVelocity,
      tolerance: toleranceFor(position),
    );
    if ((simulation.target - position.pixels).abs() < .01 && velocity == 0) {
      return null;
    }
    return simulation;
  }
}

class _WavePose extends ChangeNotifier {
  _WavePose(
    this._count,
    this.logicalFocus,
    this.entrance, {
    required this._repeatFill,
  });
  final Animation<double> entrance;
  int _count;
  bool _repeatFill;
  bool get repeatFill => _repeatFill;
  set repeatFill(bool value) {
    if (_repeatFill == value) return;
    _repeatFill = value;
    revision++;
  }

  int revision = 0;
  int get count => _count;
  set count(int value) {
    if (_count == value) return;
    _count = value;
    revision++;
  }

  double logicalFocus;
  double viewport = 340, pitch = pitchFor(340);
  bool _configured = false;
  int edgeSlots = 15;
  bool holdLayoutFocus = false;
  void inkChanged() {
    revision++;
    notifyListeners();
  }

  double inkStart(int index) => ((index - focus + 8) / 24).clamp(0.0, .65);
  double inkProgress(int index) => Interval(
    inkStart(index),
    inkStart(index) + .35,
    curve: Curves.easeOutCubic,
  ).transform(entrance.value.clamp(0.0, 1.0));
  double loadingLift(int index) => entrance.value >= 1
      ? 0
      : archiveLoadingLift(
          index - focus,
          SchedulerBinding.instance.currentSystemFrameTimeStamp,
          1 - inkProgress(index),
        );
  double get focus => count < 2
      ? 0
      : repeatFill
      ? edgeSlots + logicalFocus % count
      : logicalFocus;
  int get poolCount => count < 2 || !repeatFill ? count : count + edgeSlots * 2;
  int bookIndex(int slot) => count < 2
      ? 0
      : repeatFill
      ? (slot - edgeSlots) % count
      : slot.clamp(0, count - 1);
  int logicalLane(int slot) => count < 2
      ? 0
      : repeatFill
      ? (logicalFocus / count).floor() * count + slot - edgeSlots
      : slot.clamp(0, count - 1);
  static const depthX = ArchiveShelfGeometry.depthX;
  static const depthY = ArchiveShelfGeometry.depthY;
  static const radius = 1.48;
  static double spineFor(double width) => ArchiveShelfGeometry.spineFor(width);
  static double pitchFor(double width) => ArchiveShelfGeometry.pitchFor(width);
  static double coverFor(double width) => ArchiveShelfGeometry.coverFor(width);
  static double footprint(double spine, double cover) =>
      ArchiveShelfGeometry.footprint(spine, cover);
  static double pullDistance(double cover) =>
      ArchiveShelfGeometry.pullDistance(cover);
  static double baseY(double cover, double height) =>
      ArchiveShelfGeometry.baseY(cover, height);
  void configure(double width, double nextPitch) {
    if (_configured && viewport == width && pitch == nextPitch) return;
    _configured = true;
    viewport = width;
    pitch = nextPitch;
    final cover = coverFor(width);
    edgeSlots =
        ((width / 2 +
                    footprint(spineFor(width), cover) +
                    pullDistance(cover) * depthX) /
                pitch)
            .ceil() +
        2;
    revision++;
  }

  void move(double next, {bool notify = true}) {
    if (count < 2) next = 0;
    if (!repeatFill && count > 1) next = next.clamp(0, count - 1).toDouble();
    if ((logicalFocus - next).abs() < .000001) return;
    logicalFocus = next;
    revision++;
    if (notify) notifyListeners();
  }

  double railY(int slot) {
    return ArchiveShelfGeometry.railY(slot - focus);
  }

  double weight(int index) {
    final distance = (index - focus).abs();
    if (distance >= radius) return 0;
    final value = math.cos(distance / radius * math.pi / 2);
    // The loading skeleton already shows this crest. Keep its pose throughout
    // the ink handoff instead of dropping the selected book then lifting it.
    return value * value;
  }
}

class _ShelfRow extends MultiChildRenderObjectWidget {
  const _ShelfRow({
    required this.offset,
    required this.pose,
    required this.spine,
    required this.cover,
    required this.height,
    required this.dark,
    required this.firstSlot,
    required this.slotCount,
    required super.children,
  });
  final ViewportOffset offset;
  final _WavePose pose;
  final double spine, cover, height;
  final bool dark;
  final int firstSlot, slotCount;
  @override
  RenderObject createRenderObject(BuildContext context) =>
      ArchiveShelfSceneRender._(
        offset,
        pose,
        spine,
        cover,
        height,
        dark,
        firstSlot,
        slotCount,
      );
  @override
  void updateRenderObject(
    BuildContext context,
    ArchiveShelfSceneRender renderObject,
  ) => renderObject._update(
    offset,
    pose,
    spine,
    cover,
    height,
    dark,
    firstSlot,
    slotCount,
  );
}

class _ShelfParentData extends ContainerBoxParentData<RenderBox> {}

/// Parallel x-separated solids have a fixed occlusion order. Sorting by lift or
/// selected index causes impossible intersections and mid-scroll order pops.
class ArchiveShelfSceneRender extends RenderBox
    with ContainerRenderObjectMixin<RenderBox, _ShelfParentData>
    implements PaperCoverOcclusionSource {
  ArchiveShelfSceneRender._(
    this._scrollOffset,
    this._pose,
    this._spine,
    this._cover,
    this._height,
    this._dark,
    this._firstSlot,
    this._slotCount,
  );
  ViewportOffset _scrollOffset;
  _WavePose _pose;
  double _spine, _cover, _height;
  bool _dark;
  int _firstSlot, _slotCount;
  final _boxes = <RenderBox>[];
  final _x = <double>[];
  final _clipLayer = LayerHandle<ClipRectLayer>();
  int debugLayoutCount = 0;

  /// Static shadow ink is shared by every translated volume in this scene.
  @visibleForTesting
  int debugFloorShaderBuildCount = 0;
  Path? _floorPath;
  Paint? _floorPaint;

  /// Number of visibility projections, shared by paint, touch and cover flight.
  @visibleForTesting
  int debugVisibilityCount = 0;
  int _visibleRevision = -1;
  List<int> _visibleSlots = const [];
  double get debugFocus => _pose.logicalFocus;
  int get debugCentreSlot => _pose.focus.round();
  int get debugPoolCount => _pose.poolCount;
  double get debugPitch => _pose.pitch;
  int get debugBuiltCount => _boxes.length;
  List<int> get debugBuiltSlots => [
    for (final box in _boxes) (box as ArchiveBoxDepthRender)._index,
  ];
  List<int> get debugPaintOrder => [
    for (final i in _visible) (_boxes[i] as ArchiveBoxDepthRender)._index,
  ];
  @override
  Path? coverOcclusion(RenderBox face) {
    RenderObject? volume = face;
    while (volume != null && volume.parent != this) {
      volume = volume.parent;
    }
    if (volume is! RenderBox) return null;
    final slot = _boxes.indexOf(volume);
    if (slot < 0) return null;
    final faceBounds = MatrixUtils.transformRect(
      face.getTransformTo(null),
      Offset.zero & face.size,
    );
    var mask = Path();
    var occluded = false;
    for (final i in _visible.where((i) => i > slot)) {
      final box = _boxes[i] as ArchiveBoxDepthRender;
      final path = box._solidPath.transform(box.getTransformTo(null).storage);
      if (!path.getBounds().overlaps(faceBounds)) continue;
      // Top and side faces have opposite winding. A mere addPath can cancel
      // overlapping foreground silhouettes and expose holes in the flight.
      mask = Path.combine(PathOperation.union, mask, path);
      occluded = true;
    }
    return occluded ? mask : null;
  }

  Rect get _viewport => Offset.zero & size;
  Offset _offset(int i) => Offset(
    _x[i] - _pose.focus * _pose.pitch,
    _pose.railY((_boxes[i] as ArchiveBoxDepthRender)._index),
  );
  List<int> get _visible {
    if (_visibleRevision == _pose.revision) return _visibleSlots;
    _visibleRevision = _pose.revision;
    debugVisibilityCount++;
    return _visibleSlots = [
      for (var i = 0; i < _boxes.length; i++)
        if (_viewport.overlaps(_boxes[i].paintBounds.shift(_offset(i)))) i,
    ];
  }

  @override
  void setupParentData(RenderBox child) {
    if (child.parentData is! _ShelfParentData) {
      child.parentData = _ShelfParentData();
    }
  }

  void _update(
    ViewportOffset offset,
    _WavePose pose,
    double spine,
    double cover,
    double height,
    bool dark,
    int firstSlot,
    int slotCount,
  ) {
    final geometryChanged =
        _scrollOffset != offset ||
        _pose != pose ||
        _spine != spine ||
        _cover != cover ||
        _height != height ||
        _firstSlot != firstSlot ||
        _slotCount != slotCount;
    final inkChanged = _dark != dark;
    final surfaceChanged = _spine != spine || _cover != cover || inkChanged;
    _scrollOffset = offset;
    if (_pose != pose) {
      if (attached) _pose.removeListener(_changed);
      _pose = pose;
      if (attached) _pose.addListener(_changed);
    }
    _spine = spine;
    _cover = cover;
    _height = height;
    _dark = dark;
    _firstSlot = firstSlot;
    _slotCount = slotCount;
    if (surfaceChanged) {
      _floorPath = null;
      _floorPaint = null;
    }
    if (geometryChanged) {
      _visibleRevision = -1;
      markNeedsLayout();
    } else if (inkChanged) {
      markNeedsPaint();
    }
  }

  void _changed() {
    markNeedsPaint();
    markNeedsSemanticsUpdate();
  }

  @override
  void attach(PipelineOwner owner) {
    super.attach(owner);
    _pose.addListener(_changed);
  }

  @override
  void detach() {
    _pose.removeListener(_changed);
    super.detach();
  }

  @override
  void performLayout() {
    _visibleRevision = -1;
    size = constraints.biggest;
    _scrollOffset.applyViewportDimension(size.width);
    // A resized viewport may correct its pixels on the first attempt. Accept
    // the new dimensions before deriving the pool's fractional lane.
    for (var attempt = 0; attempt < 3; attempt++) {
      if (_scrollOffset.applyContentDimensions(
        _pose.count < 2 || !_pose.repeatFill ? 0 : double.negativeInfinity,
        _pose.count < 2
            ? 0
            : _pose.repeatFill
            ? double.infinity
            : (_pose.count - 1) * _pose.pitch,
      )) {
        break;
      }
    }
    _pose.move(_scrollOffset.pixels / _pose.pitch, notify: false);
    _boxes.clear();
    _x.clear();
    final start =
        _pose.viewport / 2 -
        _WavePose.footprint(_spine, _cover) / 2 +
        _WavePose.pullDistance(_cover) * _WavePose.depthX;
    var child = firstChild;
    while (child != null) {
      child.layout(BoxConstraints.tight(Size(_spine + _cover, size.height)));
      _x.add(start + (child as ArchiveBoxDepthRender)._index * _pose.pitch);
      _boxes.add(child);
      child = childAfter(child);
    }
    debugLayoutCount++;
  }

  void _floor(Canvas canvas, Offset offset, List<int> visible) {
    if (_floorPath == null) {
      final depth = Offset(
        _cover * _WavePose.depthX,
        _cover * _WavePose.depthY,
      );
      const tail = Offset(-10, 10);
      final front = Offset(_spine, 1.4);
      _floorPath = Path()
        ..addPolygon([
          Offset.zero,
          front,
          front + depth,
          depth,
          depth + tail,
          tail,
        ], true);
      final ink = _dark ? ShelfMaterial.milk : const Color(0xFF8A806B);
      _floorPaint = Paint()
        ..shader = LinearGradient(
          begin: Alignment.topRight,
          end: Alignment.bottomLeft,
          colors: [ink.withValues(alpha: .10), ink.withValues(alpha: 0)],
        ).createShader(_floorPath!.getBounds());
      debugFloorShaderBuildCount++;
    }
    for (final i in visible) {
      final origin =
          offset +
          _offset(i) +
          Offset(0, _WavePose.baseY(_cover, _height) + _height);
      canvas.save();
      canvas.translate(origin.dx, origin.dy);
      canvas.drawPath(_floorPath!, _floorPaint!);
      canvas.restore();
    }
  }

  @override
  void paint(PaintingContext context, Offset offset) {
    final visible = _visible;
    _clipLayer.layer = context.pushClipRect(
      needsCompositing,
      offset,
      _viewport,
      (context, offset) {
        _floor(context.canvas, offset, visible);
        for (final i in visible) {
          context.paintChild(_boxes[i], offset + _offset(i));
        }
      },
      oldLayer: _clipLayer.layer,
    );
  }

  @override
  bool hitTestChildren(BoxHitTestResult result, {required Offset position}) {
    for (final i in _visible.reversed) {
      if (result.addWithPaintOffset(
        offset: _offset(i),
        position: position,
        hitTest: (result, position) =>
            _boxes[i].hitTest(result, position: position),
      )) {
        return true;
      }
    }
    return false;
  }

  @override
  void applyPaintTransform(RenderBox child, Matrix4 transform) {
    final offset = _offset(_boxes.indexOf(child));
    transform.translateByDouble(offset.dx, offset.dy, 0, 1);
  }

  @override
  Rect describeApproximatePaintClip(RenderObject child) => _viewport;

  @override
  void dispose() {
    _clipLayer.layer = null;
    super.dispose();
  }
}

class _BookVolume extends MultiChildRenderObjectWidget {
  const _BookVolume({
    super.key,
    required this.pose,
    required this.index,
    required this.spine,
    required this.cover,
    required this.height,
    required this.dark,
    required this.label,
    required this.direction,
    required this.onTap,
    required super.children,
  });
  final _WavePose pose;
  final int index;
  final double spine, cover, height;
  final bool dark;
  final String label;
  final TextDirection direction;
  final VoidCallback onTap;
  @override
  RenderObject createRenderObject(BuildContext context) =>
      ArchiveBoxDepthRender._(
        pose,
        index,
        spine,
        cover,
        height,
        dark,
        label,
        direction,
        onTap,
      );
  @override
  void updateRenderObject(
    BuildContext context,
    ArchiveBoxDepthRender renderObject,
  ) => renderObject._update(
    pose,
    index,
    spine,
    cover,
    height,
    dark,
    label,
    direction,
    onTap,
  );
}

class _ArchiveCapHitEntry extends BoxHitTestEntry {
  _ArchiveCapHitEntry(super.target, super.localPosition);
}

/// The top, front and projected side share exactly the same physical vertices.
class ArchiveBoxDepthRender extends RenderBox
    with ContainerRenderObjectMixin<RenderBox, _ShelfParentData> {
  ArchiveBoxDepthRender._(
    this._pose,
    this._index,
    this._spine,
    this._cover,
    this._height,
    this._dark,
    this._label,
    this._direction,
    this._onTap,
  ) {
    _capTap = TapGestureRecognizer(debugOwner: this)
      ..onTap = () {
        _onTap();
      };
  }
  _WavePose _pose;
  int _index;
  double _spine, _cover, _height;
  bool _dark;
  String _label;
  TextDirection _direction;
  VoidCallback _onTap;
  late final TapGestureRecognizer _capTap;
  final _spineLayer = LayerHandle<TransformLayer>();
  final _coverLayer = LayerHandle<TransformLayer>();
  int debugLayoutCount = 0;

  /// Translation does not change a book's cap geometry or gradient.
  @visibleForTesting
  int debugTopShaderBuildCount = 0;
  Path? _capPath;
  Paint? _capFill, _capStroke;

  /// Culling, paint and accessibility reuse one exact pose per wave revision.
  @visibleForTesting
  int debugGeometryCount = 0;
  int _geometryRevision = -1;
  Offset _resolvedOrigin = Offset.zero;
  Matrix4? _resolvedBase, _resolvedCover;
  Path? _resolvedTop;
  double get debugOpenFraction => _pose.weight(_index);
  bool get debugSelected => _pose.focus.round() == _index;
  Matrix4 get debugTransform => _coverTransform.clone();
  Matrix4 get debugSpineTransform => _base.clone();
  double get debugFootprint => _WavePose.footprint(_spine, _cover);
  Offset get _origin {
    if (_geometryRevision != _pose.revision) {
      _geometryRevision = _pose.revision;
      debugGeometryCount++;
      _resolvedBase = null;
      _resolvedCover = null;
      _resolvedTop = null;
      _resolvedOrigin =
          ArchiveShelfGeometry.origin(_cover, _height, debugOpenFraction) +
          Offset(0, _pose.loadingLift(_index));
    }
    return _resolvedOrigin;
  }

  Matrix4 get _base {
    final origin = _origin;
    return _resolvedBase ??= Matrix4.identity()
      ..setEntry(0, 2, _WavePose.depthX)
      ..setEntry(1, 2, _WavePose.depthY)
      ..setEntry(1, 0, .10)
      ..setEntry(0, 3, origin.dx)
      ..setEntry(1, 3, origin.dy);
  }

  Matrix4 get _coverTransform {
    final origin = _origin;
    return _resolvedCover ??= Matrix4.identity()
      ..setEntry(0, 0, _WavePose.depthX)
      ..setEntry(1, 0, _WavePose.depthY)
      ..setEntry(0, 3, origin.dx + _spine)
      ..setEntry(1, 3, origin.dy + _spine * .10);
  }

  Path get _topPath {
    final origin = _origin;
    final front = Offset(_spine, _spine * .10);
    final depth = Offset(_cover * _WavePose.depthX, _cover * _WavePose.depthY);
    return _resolvedTop ??= Path()
      ..addPolygon([
        origin,
        origin + front,
        origin + front + depth,
        origin + depth,
      ], true);
  }

  Path get _solidPath {
    List<Offset> face(Matrix4 matrix, double width) => [
      for (final point in [
        Offset.zero,
        Offset(width, 0),
        Offset(width, _height),
        Offset(0, _height),
      ])
        MatrixUtils.transformPoint(matrix, point),
    ];
    return Path()
      ..addPath(_topPath, Offset.zero)
      ..addPolygon(face(_base, _spine), true)
      ..addPolygon(face(_coverTransform, _cover), true);
  }

  @override
  void setupParentData(RenderBox child) {
    if (child.parentData is! _ShelfParentData) {
      child.parentData = _ShelfParentData();
    }
  }

  void _update(
    _WavePose pose,
    int index,
    double spine,
    double cover,
    double height,
    bool dark,
    String label,
    TextDirection direction,
    VoidCallback onTap,
  ) {
    _geometryRevision = -1;
    if (_spine != spine || _cover != cover || _dark != dark) {
      _capPath = null;
      _capFill = null;
      _capStroke = null;
    }
    if (_pose != pose) {
      if (attached) _pose.removeListener(_changed);
      _pose = pose;
      if (attached) _pose.addListener(_changed);
    }
    if (_spine != spine || _cover != cover || _height != height) {
      _spine = spine;
      _cover = cover;
      _height = height;
      markNeedsLayout();
    }
    _index = index;
    _dark = dark;
    _label = label;
    _direction = direction;
    _onTap = onTap;
    _changed();
  }

  void _changed() {
    markNeedsPaint();
    markNeedsSemanticsUpdate();
  }

  @override
  void attach(PipelineOwner owner) {
    super.attach(owner);
    _pose.addListener(_changed);
  }

  @override
  void detach() {
    _pose.removeListener(_changed);
    _spineLayer.layer = null;
    _coverLayer.layer = null;
    super.detach();
  }

  @override
  void performLayout() {
    size = constraints.biggest;
    firstChild!.layout(BoxConstraints.tight(Size(_spine, _height)));
    lastChild!.layout(BoxConstraints.tight(Size(_cover, _height)));
    debugLayoutCount++;
  }

  @override
  void paint(PaintingContext context, Offset offset) {
    _coverLayer.layer = context.pushTransform(
      needsCompositing,
      offset,
      _coverTransform,
      (context, offset) => context.paintChild(lastChild!, offset),
      oldLayer: _coverLayer.layer,
    );
    if (_capPath == null) {
      final front = Offset(_spine, _spine * .10);
      final depth = Offset(
        _cover * _WavePose.depthX,
        _cover * _WavePose.depthY,
      );
      _capPath = Path()
        ..addPolygon([Offset.zero, front, front + depth, depth], true);
      _capFill = Paint()
        ..shader = LinearGradient(
          colors: _dark
              ? [ShelfMaterial.darkRaisedHigh, ShelfMaterial.darkRaised]
              : [const Color(0xFFE3DFD3), ShelfMaterial.milk],
        ).createShader(_capPath!.getBounds());
      _capStroke = Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = .4
        ..color = (_dark ? ShelfMaterial.darkMilk : const Color(0xFF9A927F))
            .withValues(alpha: .24);
      debugTopShaderBuildCount++;
    }
    final canvas = context.canvas;
    final origin = offset + _origin;
    canvas.save();
    canvas.translate(origin.dx, origin.dy);
    canvas.drawPath(_capPath!, _capFill!);
    canvas.drawPath(_capPath!, _capStroke!);
    canvas.restore();
    _spineLayer.layer = context.pushTransform(
      needsCompositing,
      offset,
      _base,
      (context, offset) => context.paintChild(firstChild!, offset),
      oldLayer: _spineLayer.layer,
    );
  }

  @override
  bool hitTestChildren(BoxHitTestResult result, {required Offset position}) =>
      result.addWithPaintTransform(
        transform: _base,
        position: position,
        hitTest: (result, position) =>
            firstChild!.hitTest(result, position: position),
      ) ||
      result.addWithPaintTransform(
        transform: _coverTransform,
        position: position,
        hitTest: (result, position) =>
            lastChild!.hitTest(result, position: position),
      );
  @override
  bool hitTest(BoxHitTestResult result, {required Offset position}) {
    if (!paintBounds.contains(position)) return false;
    if (hitTestChildren(result, position: position)) {
      result.add(BoxHitTestEntry(this, position));
      return true;
    }
    if (_topPath.contains(position)) {
      result.add(_ArchiveCapHitEntry(this, position));
      return true;
    }
    return false;
  }

  @override
  void handleEvent(PointerEvent event, covariant BoxHitTestEntry entry) {
    if (entry is _ArchiveCapHitEntry && event is PointerDownEvent) {
      _capTap.addPointer(event);
    }
  }

  @override
  void applyPaintTransform(RenderBox child, Matrix4 transform) =>
      transform.multiply(child == firstChild ? _base : _coverTransform);
  @override
  Rect get paintBounds => semanticBounds.inflate(.5);
  @override
  Rect get semanticBounds {
    // Exact projected solid bounds without allocating matrices/paths for every
    // offscreen volume during each culling pass.
    final origin = _origin;
    return Rect.fromLTRB(
      origin.dx,
      origin.dy + _cover * _WavePose.depthY,
      origin.dx + debugFootprint,
      origin.dy + _height + _spine * .10,
    );
  }

  @override
  void describeSemanticsConfiguration(SemanticsConfiguration config) {
    super.describeSemanticsConfiguration(config);
    config.isSemanticBoundary = true;
    config.isButton = true;
    config.isSelected = debugSelected;
    config.label = _label;
    config.textDirection = _direction;
    config.onTap = _onTap;
    config.hint = debugSelected ? '中心档案，点按打开书籍' : '点按滚动到中心';
  }

  @override
  void dispose() {
    _capTap.dispose();
    _spineLayer.layer = null;
    _coverLayer.layer = null;
    super.dispose();
  }
}

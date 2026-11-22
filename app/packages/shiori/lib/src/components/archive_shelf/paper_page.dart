import 'package:cupertino_ui/cupertino_ui.dart';

/// A Cupertino route retains native edge-back gestures on iPad while lending
/// its full clock to the shelf's paper peel and reverse landing.
class ShioriPaperPage<T> extends Page<T> {
  const ShioriPaperPage({required this.child, super.key});
  final Widget child;

  @override
  Route<T> createRoute(BuildContext context) => _PaperRoute<T>(
    child: child,
    reduced: MediaQuery.disableAnimationsOf(context),
    settings: this,
  );
}

class _PaperRoute<T> extends CupertinoPageRoute<T> {
  _PaperRoute({
    required Widget child,
    required this.reduced,
    required super.settings,
  }) : super(builder: (_) => RepaintBoundary(child: child));

  final bool reduced;
  @override
  Duration get transitionDuration =>
      reduced ? Duration.zero : const Duration(milliseconds: 1000);
  @override
  Duration get reverseTransitionDuration =>
      reduced ? Duration.zero : const Duration(milliseconds: 840);
}

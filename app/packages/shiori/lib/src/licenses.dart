import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

bool _registered = false;

/// Keep the source distribution's original notices visible in the app's
/// existing open-source licenses screen.
void registerShioriLicenses() {
  if (_registered) return;
  _registered = true;
  LicenseRegistry.addLicense(() async* {
    for (final file in ['NOTICE', 'LICENSE', 'GPL-3.0.txt']) {
      yield LicenseEntryWithLineBreaks(
        const ['Wenku8 Flutter v0.5.6 — adapted shelf and paper motion'],
        await rootBundle.loadString('packages/shiori/third_party/wenku8/$file'),
      );
    }
  });
}

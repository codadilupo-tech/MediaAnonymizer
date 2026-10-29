import 'package:flutter/material.dart';

import '../engine/engine.dart';
import '../settings.dart';

String plural(int n, String word) => '$n $word${n == 1 ? '' : 's'}';

String formatBytes(int bytes) {
  if (bytes < 1024) return '$bytes B';
  if (bytes < 1024 * 1024) return '${(bytes / 1024).toStringAsFixed(1)} KB';
  return '${(bytes / (1024 * 1024)).toStringAsFixed(1)} MB';
}

/// One line describing a finished photo, e.g. "2 faces · 12 blocks removed".
String summarizeReport(Report r) => [
  plural(r.faces, 'face'),
  if (r.removed.isNotEmpty) '${plural(r.removed.length, 'block')} removed',
  if (r.timestamp != null) 'dated',
].join(' · ');

/// One line describing what the settings do.
String describeSettings(Settings s) => [
  switch (s.metadata) {
    MetadataPolicy.keep => 'Metadata kept',
    MetadataPolicy.sensitive => 'Sensitive metadata removed',
    MetadataPolicy.all => 'All metadata removed',
  },
  if (s.faces) 'faces: ${s.faceMode.label.toLowerCase()}, ${s.style.label.toLowerCase()}',
  if (s.timestamp) 'timestamp ${s.timestampPosition.label.toLowerCase()}',
].join(' · ');

void showError(BuildContext context, String message) {
  ScaffoldMessenger.of(context)
      .showSnackBar(SnackBar(content: Text(message), behavior: SnackBarBehavior.floating, showCloseIcon: true));
}

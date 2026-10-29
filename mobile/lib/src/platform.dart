import 'package:flutter/widgets.dart';
import 'package:image_picker/image_picker.dart';
import 'package:share_plus/share_plus.dart';

import 'queue.dart';

/// Device integrations: photo picker, camera and share sheet.
class PlatformServices {
  const PlatformServices();

  static final _picker = ImagePicker();

  /// Lets the user choose photos. `requestFullMetadata` keeps the original
  /// file (with its metadata) instead of a re-encoded copy where possible.
  Future<List<PhotoItem>> pickPhotos() async {
    final files = await _picker.pickMultiImage(requestFullMetadata: true);
    return [for (final f in files) PhotoItem(f.path, name: f.name)];
  }

  Future<PhotoItem?> takePhoto() async {
    final file = await _picker.pickImage(source: ImageSource.camera, requestFullMetadata: true);
    return file == null ? null : PhotoItem(file.path, name: file.name);
  }

  /// Opens the share sheet for [paths]. [context] anchors the popover on iPad.
  Future<void> share(BuildContext context, List<String> paths) async {
    final box = context.findRenderObject() as RenderBox?;
    await SharePlus.instance.share(
      ShareParams(
        files: [for (final path in paths) XFile(path)],
        sharePositionOrigin: box == null ? null : box.localToGlobal(Offset.zero) & box.size,
      ),
    );
  }
}

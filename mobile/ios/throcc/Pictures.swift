import ImageIO
import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

struct Avatar: View {
    let picture: UIImage?
    /// The letter shown while there is no picture.
    let initial: String
    let size: CGFloat

    var body: some View {
        Group {
            if let picture {
                Image(uiImage: picture)
                    .resizable()
                    .scaledToFill()
            } else {
                Text(initial)
                    .font(.system(size: size * 0.45, weight: .medium))
                    .foregroundStyle(.muted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(ThemeColor.raised)
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .accessibilityHidden(true)
    }
}

/// Someone's picture, or a ban symbol for a blocked person.
struct PersonPicture: View {
    @Environment(AppModel.self) private var model
    let person: Person
    let size: CGFloat

    var body: some View {
        if person.blocked {
            Image(systemName: "nosign")
                .font(.system(size: size * 0.5))
                .foregroundStyle(.muted)
                .frame(width: size, height: size)
                .background(ThemeColor.raised, in: Circle())
                .accessibilityHidden(true)
        } else {
            Avatar(picture: person.avatar.flatMap { model.pictures[$0] }, initial: person.initial, size: size)
        }
    }
}

struct PictureEditor: View {
    @Environment(AppModel.self) private var model
    @Binding var picture: Data?
    let name: String
    @State private var picked: PhotosPickerItem?

    var body: some View {
        HStack(spacing: 16) {
            Avatar(picture: model.picture(in: picture), initial: initial(name: name), size: 72)
                .opacity(picked == nil ? 1 : 0.3)
                .overlay {
                    if picked != nil {
                        ProgressView()
                    }
                }
            VStack(alignment: .leading, spacing: 0) {
                PhotosPicker(selection: $picked, matching: .images) {
                    Text("Choose Picture…").touchTarget()
                }
                if picture != nil {
                    Button(role: .destructive) { picture = nil } label: {
                        Text("Remove Picture").foregroundStyle(.bad).touchTarget()
                    }
                }
            }
        }
        .buttonStyle(.borderless)
        .task(id: picked) {
            guard let picked else { return }
            let chosen = try? await picked.loadTransferable(type: Data.self)
            let prepared = await Task.detached { Result { try profilePicture(from: chosen) } }.value
            guard !Task.isCancelled, self.picked == picked else { return }
            switch prepared {
            case .success(let bytes): picture = bytes
            case .failure(let error): model.notice = error.explanation
            }
            self.picked = nil
        }
    }
}

/// The picture in `file` at no more than 256 px a side, or nil where it claims over 512 px
/// a side or can't be read.
func shownPicture(in file: Data) -> UIImage? {
    guard let source = CGImageSourceCreateWithData(file as CFData, nil),
          let claimed = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
          let width = claimed[kCGImagePropertyPixelWidth] as? Int,
          let height = claimed[kCGImagePropertyPixelHeight] as? Int,
          max(width, height) <= 512
    else { return nil }
    return thumbnail(of: source, within: 256).map(UIImage.init(cgImage:))
}

/// The first picture in `source`, upright and decoded at no more than `side` pixels a side.
private func thumbnail(of source: CGImageSource, within side: Int) -> CGImage? {
    let options: [CFString: Any] = [
        kCGImageSourceCreateThumbnailFromImageAlways: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: side,
    ]
    return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
}

/// The picture in `file` made into a profile picture. It is decoded here, which reads formats
/// such as HEIC, and handed to the core as an 8-bit sRGB PNG no larger than the core needs: the
/// core reads no colour profile, and crops to a square anyway.
func profilePicture(from file: Data?) throws -> Data {
    guard let file,
          let source = CGImageSourceCreateWithData(file as CFData, nil),
          let claimed = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
          let width = claimed[kCGImagePropertyPixelWidth] as? Int,
          let height = claimed[kCGImagePropertyPixelHeight] as? Int,
          min(width, height) > 0
    else { throw UnreadablePicture() }
    let (short, long) = (min(width, height), max(width, height))
    let longSide = short <= maxChosenShortSide ? long : long * maxChosenShortSide / short
    guard let decoded = thumbnail(of: source, within: longSide),
          let png = standardPNG(of: decoded)
    else { throw UnreadablePicture() }
    return try preparePicture(file: png)
}

/// The middle of `image`, at most `maxChosenElongation` times its short side along its long
/// side, as an 8-bit sRGB PNG. Core Graphics rather than UIKit, as this runs off the main actor.
private func standardPNG(of image: CGImage) -> Data? {
    let longest = maxChosenElongation * min(image.width, image.height)
    let (width, height) = (min(image.width, longest), min(image.height, longest))
    guard let sRGB = CGColorSpace(name: CGColorSpace.sRGB),
          let context = CGContext(
              data: nil,
              width: width,
              height: height,
              bitsPerComponent: 8,
              bytesPerRow: 0,
              space: sRGB,
              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
          )
    else { return nil }
    let centred = CGRect(x: (width - image.width) / 2, y: (height - image.height) / 2, width: image.width, height: image.height)
    context.draw(image, in: centred)
    let png = NSMutableData()
    guard let drawn = context.makeImage(),
          let destination = CGImageDestinationCreateWithData(png as CFMutableData, UTType.png.identifier as CFString, 1, nil)
    else { return nil }
    CGImageDestinationAddImage(destination, drawn, nil)
    return CGImageDestinationFinalize(destination) ? png as Data : nil
}

/// The short side a chosen picture is scaled down to before the core squares it.
private let maxChosenShortSide = 512

/// How many times its short side a chosen picture's long side keeps at most.
private let maxChosenElongation = 4

private struct UnreadablePicture: LocalizedError {
    var errorDescription: String? { "That picture couldn’t be read. Choose another one." }
}

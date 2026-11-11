import PhotosUI
import SwiftUI

struct Avatar: View {
    let picture: UIImage?
    let name: String
    let size: CGFloat

    var body: some View {
        Group {
            if let picture {
                Image(uiImage: picture)
                    .resizable()
                    .scaledToFill()
            } else {
                Text(name.prefix(1).uppercased())
                    .font(.system(size: size * 0.45, weight: .medium))
                    .foregroundStyle(.muted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(ThemeColor.raised)
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
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
        } else {
            Avatar(picture: person.avatar.flatMap { model.pictures[$0] }, name: person.name, size: size)
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
            Avatar(picture: model.picture(in: picture), name: name, size: 72)
            VStack(alignment: .leading, spacing: 12) {
                PhotosPicker("Choose Picture…", selection: $picked, matching: .images)
                if picture != nil {
                    Button("Remove", role: .destructive) { picture = nil }
                }
            }
        }
        .buttonStyle(.borderless)
        .task(id: picked) {
            guard let picked else { return }
            let chosen = try? await picked.loadTransferable(type: Data.self)
            if let fitted = chosen.flatMap(UIImage.init(data:)).flatMap(profilePicture(from:)) {
                picture = fitted
            } else {
                model.notice = "That picture couldn’t be read."
            }
            self.picked = nil
        }
    }
}

/// The centre square of `image` at 256 px, as a JPEG of at most 64 KiB.
func profilePicture(from image: UIImage) -> Data? {
    let side: CGFloat = 256
    let scale = side / min(image.size.width, image.size.height)
    let scaled = CGSize(width: image.size.width * scale, height: image.size.height * scale)
    let format = UIGraphicsImageRendererFormat()
    format.scale = 1
    format.opaque = true
    let square = UIGraphicsImageRenderer(size: CGSize(width: side, height: side), format: format).image { _ in
        let origin = CGPoint(x: (side - scaled.width) / 2, y: (side - scaled.height) / 2)
        image.draw(in: CGRect(origin: origin, size: scaled))
    }
    let qualities = stride(from: 0.9, through: 0.1, by: -0.2)
    return qualities.lazy.compactMap { square.jpegData(compressionQuality: $0) }.first { $0.count <= Int(limits().pictureBytes) }
}

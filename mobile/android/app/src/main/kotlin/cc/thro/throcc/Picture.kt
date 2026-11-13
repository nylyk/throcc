package cc.thro.throcc

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.ColorSpace
import android.graphics.ImageDecoder
import android.graphics.Rect
import android.net.Uri
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import cc.thro.throcc.core.preparePicture
import java.io.ByteArrayOutputStream
import java.io.IOException
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

private const val PICTURE_SIDE = 256
private const val MAX_SHOWN_SIDE = 512
private const val MAX_CHOSEN_SHORT_SIDE = 512

/** How many times its short side a chosen picture's long side keeps at most. */
private const val MAX_CHOSEN_ELONGATION = 4

/** A picture file with the image it decodes to, which is null for a file that can't be shown. */
class Picture(val file: ByteArray) {
    val image = file.toImage()
}

/** The pictures in `files` by the hex of their key, taken from `kept` where it holds the same file. */
fun decoded(files: List<Pair<ByteArray, ByteArray?>>, kept: Map<String, Picture>): Map<String, Picture> =
    files.mapNotNull { (key, file) ->
        val name = key.toHexString()
        file?.let { name to (kept[name]?.takeIf { old -> old.file.contentEquals(it) } ?: Picture(it)) }
    }.toMap()

/** The image in this file at no more than 256 px a side, or null where it is over 512 px or unreadable. */
fun ByteArray.toImage(): ImageBitmap? {
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(this, 0, size, bounds)
    val side = maxOf(bounds.outWidth, bounds.outHeight)
    if (minOf(bounds.outWidth, bounds.outHeight) < 1 || side > MAX_SHOWN_SIDE) return null
    val shrunk = BitmapFactory.Options().apply {
        inSampleSize = if (side > PICTURE_SIDE) MAX_SHOWN_SIDE / PICTURE_SIDE else 1
    }
    return BitmapFactory.decodeByteArray(this, 0, size, shrunk)?.asImageBitmap()
}

/**
 * The image at `uri` as a picture any server takes, or null if Android can't read it. The core makes the picture,
 * and throws why where it can't; this only turns what Android reads into an 8-bit sRGB PNG of at most 512 px on its
 * short side and four times that on its long side, as the core reads no colour profile and crops to a square anyway.
 */
suspend fun profilePicture(context: Context, uri: Uri): ByteArray? = withContext(Dispatchers.IO) {
    val image = try {
        ImageDecoder.decodeBitmap(ImageDecoder.createSource(context.contentResolver, uri)) { decoder, info, _ ->
            val scale = minOf(1f, MAX_CHOSEN_SHORT_SIDE.toFloat() / minOf(info.size.width, info.size.height))
            val width = (info.size.width * scale).roundToInt().coerceAtLeast(1)
            val height = (info.size.height * scale).roundToInt().coerceAtLeast(1)
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
            decoder.setTargetColorSpace(SRGB)
            decoder.setTargetSize(width, height)
            decoder.crop = centred(width, height, MAX_CHOSEN_ELONGATION * minOf(width, height))
        }
    } catch (_: IOException) {
        return@withContext null
    }
    val png = ByteArrayOutputStream()
    image.inStandardRgb().compress(Bitmap.CompressFormat.PNG, 100, png)
    preparePicture(png.toByteArray())
}

/** The middle of a `width` by `height` image, at most `longest` on either side. */
private fun centred(width: Int, height: Int, longest: Int): Rect {
    val (keptWidth, keptHeight) = minOf(width, longest) to minOf(height, longest)
    val (left, top) = (width - keptWidth) / 2 to (height - keptHeight) / 2
    return Rect(left, top, left + keptWidth, top + keptHeight)
}

/** This bitmap as 8-bit sRGB, which a wide or deep image (such as a 10-bit HEIF) may not decode to. */
private fun Bitmap.inStandardRgb(): Bitmap {
    if (config == Bitmap.Config.ARGB_8888 && colorSpace == SRGB) return this
    val standard = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888, true, SRGB)
    Canvas(standard).drawBitmap(this, 0f, 0f, null)
    return standard
}

private val SRGB = ColorSpace.get(ColorSpace.Named.SRGB)

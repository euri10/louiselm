package dev.louiselm.capture

import android.app.Activity
import android.content.DialogInterface
import android.os.Looper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowAlertDialog

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "en")
class PairingConfirmationTest {
    @Test
    fun sameReceiverOffersExplicitCredentialRenewalWithoutAutomaticMutation() {
        // C11 / louiselm-ltzy: fresh same-SPKI QR retained a revoked credential.
        Robolectric.buildActivity(Activity::class.java).setup().use { controller ->
            val confirmed = mutableListOf<PairingTransition>()
            showPairingConfirmation(
                controller.get(), PairingTransition.ENDPOINT_UPDATE, 1, "https://192.0.2.1:7393",
                onConfirm = confirmed::add, onCancel = {},
            )
            val dialog = ShadowAlertDialog.getLatestAlertDialog()
            assertNotNull("A same-receiver QR must offer credential recovery", dialog)
            assertTrue("Scanning alone must not change pairing", confirmed.isEmpty())
            dialog.getButton(DialogInterface.BUTTON_NEUTRAL).performClick()
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf("CREDENTIAL_RENEWAL"), confirmed.map { it.name })
        }
    }

    @Test
    fun keepPairingSelectsEndpointUpdateInsteadOfReplacingCredential() {
        Robolectric.buildActivity(Activity::class.java).setup().use { controller ->
            val confirmed = mutableListOf<PairingTransition>()
            showPairingConfirmation(
                controller.get(), PairingTransition.ENDPOINT_UPDATE, 1, "https://192.0.2.1:7393",
                onConfirm = confirmed::add, onCancel = {},
            )
            val dialog = ShadowAlertDialog.getLatestAlertDialog()
            assertTrue(confirmed.isEmpty())
            val keep = dialog.getButton(DialogInterface.BUTTON_POSITIVE)
            assertEquals(controller.get().getString(R.string.keep_pairing), keep.text.toString())
            keep.performClick()
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf(PairingTransition.ENDPOINT_UPDATE), confirmed)
        }
    }

    @Test
    fun cancelOrDismissDoesNotAuthorizeAnyPairingChange() {
        Robolectric.buildActivity(Activity::class.java).setup().use { controller ->
            for (dismiss in listOf(false, true)) {
                val confirmed = mutableListOf<PairingTransition>()
                var cancellations = 0
                showPairingConfirmation(
                    controller.get(), PairingTransition.ENDPOINT_UPDATE, 1, "https://192.0.2.1:7393",
                    onConfirm = confirmed::add, onCancel = { cancellations++ },
                )
                val dialog = ShadowAlertDialog.getLatestAlertDialog()
                if (dismiss) dialog.cancel() else dialog.getButton(DialogInterface.BUTTON_NEGATIVE).performClick()
                shadowOf(Looper.getMainLooper()).idle()
                assertTrue(confirmed.isEmpty())
                assertEquals(1, cancellations)
            }
        }
    }

    @Test
    fun firstPairAndReceiverMigrationStillRequireTheirOwnConfirmation() {
        Robolectric.buildActivity(Activity::class.java).setup().use { controller ->
            for (transition in listOf(PairingTransition.FIRST_PAIR, PairingTransition.RECEIVER_MIGRATION)) {
                val confirmed = mutableListOf<PairingTransition>()
                showPairingConfirmation(
                    controller.get(), transition, 1, "https://192.0.2.1:7393",
                    onConfirm = confirmed::add, onCancel = {},
                )
                val dialog = ShadowAlertDialog.getLatestAlertDialog()
                assertTrue(confirmed.isEmpty())
                assertFalse(dialog.getButton(DialogInterface.BUTTON_NEUTRAL).isShown)
                dialog.getButton(DialogInterface.BUTTON_POSITIVE).performClick()
                shadowOf(Looper.getMainLooper()).idle()
                assertEquals(listOf(transition), confirmed)
            }
        }
    }
}

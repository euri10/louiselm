package dev.louiselm.capture

import android.content.pm.ApplicationInfo
import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.xmlpull.v1.XmlPullParser

@RunWith(RobolectricTestRunner::class)
class BackupPolicyTest {
    @Test
    @Config(sdk = [28, 34])
    fun backupRemainsDisabled() {
        val application = RuntimeEnvironment.getApplication()
        assertEquals(0, application.applicationInfo.flags and ApplicationInfo.FLAG_ALLOW_BACKUP)
    }

    @Test
    @Config(sdk = [34])
    fun packagedRulesExcludeAppDataFromCloudAndDeviceTransfer() {
        val application = RuntimeEnvironment.getApplication()
        val androidNamespace = "http://schemas.android.com/apk/res/android"
        val rulesId = application.assets.openXmlResourceParser("AndroidManifest.xml").use { manifest ->
            while (manifest.next() != XmlPullParser.END_DOCUMENT) {
                if (manifest.eventType == XmlPullParser.START_TAG && manifest.name == "application") {
                    return@use manifest.getAttributeResourceValue(androidNamespace, "dataExtractionRules", 0)
                }
            }
            0
        }
        assertTrue("Manifest must reference packaged data extraction rules", rulesId != 0)
        val exclusions = mutableMapOf<String, MutableSet<String>>()
        application.resources.getXml(rulesId).use { rules ->
            var channel = ""
            while (rules.next() != XmlPullParser.END_DOCUMENT) {
                if (rules.eventType != XmlPullParser.START_TAG) continue
                when (rules.name) {
                    "cloud-backup", "device-transfer" -> {
                        channel = rules.name
                        exclusions[channel] = mutableSetOf()
                    }
                    "exclude" -> {
                        assertEquals("Exclude the entire storage domain", ".", rules.getAttributeValue(null, "path"))
                        exclusions.getValue(channel).add(rules.getAttributeValue(null, "domain"))
                    }
                    "include" -> fail("Backup opt-out must not include data")
                }
            }
        }
        val domains = setOf(
            "root", "file", "database", "sharedpref", "external",
            "device_root", "device_file", "device_database", "device_sharedpref",
        )
        assertEquals(mapOf("cloud-backup" to domains, "device-transfer" to domains), exclusions)
    }
}

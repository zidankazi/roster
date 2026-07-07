import type { Metadata } from "next";
import "./globals.css";
import { Navbar } from "./Navbar";

export const metadata: Metadata = {
  title: "roster",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>
        <Navbar />
        {children}
      </body>
    </html>
  );
}

import React from "react";
import GoBack from "../layout/GoBack";

interface AuthHeaderProps {
  title: string;
  subtitle: string;
  showBackButton?: boolean; // Added a prop to control visibility
}

const AuthHeader = ({
  title,
  subtitle,
  showBackButton = true,
}: AuthHeaderProps) => {
  return (
    <div className="w-full mb-3 sm:mb-5">
      {showBackButton && <GoBack text="Go Back" />}

      <div className="text-center">
        <h1 className="text-xl sm:text-2xl font-bold text-black tracking-tight cursor-pointer">
          {title}
        </h1>
        <p className="text-black mt-1 sm:mt-2 text-sm sm:text-base font-medium">{subtitle}</p>
      </div>
    </div>
  );
};

export default AuthHeader;
